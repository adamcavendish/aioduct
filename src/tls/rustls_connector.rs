use std::io;
use std::io::{Read as StdRead, Write as StdWrite};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper::rt::{self, Read, Write};
use rustls::pki_types::ServerName;

use super::TlsConnect;
use crate::runtime::Runtime;

pub struct RustlsConnector {
    config: Arc<rustls::ClientConfig>,
}

impl RustlsConnector {
    pub fn new(config: Arc<rustls::ClientConfig>) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &Arc<rustls::ClientConfig> {
        &self.config
    }

    pub fn with_webpki_roots() -> Self {
        let root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::new(Arc::new(config))
    }

    pub fn negotiated_protocol(tls_conn: &rustls::ClientConnection) -> Option<AlpnProtocol> {
        tls_conn.alpn_protocol().and_then(|proto| {
            if proto == b"h2" {
                Some(AlpnProtocol::H2)
            } else if proto == b"http/1.1" {
                Some(AlpnProtocol::H1)
            } else {
                None
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlpnProtocol {
    H1,
    H2,
}

impl<R: Runtime> TlsConnect<R> for RustlsConnector {
    type Stream = TlsStream<R::TcpStream>;

    fn connect(
        &self,
        server_name: &str,
        stream: R::TcpStream,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<Self::Stream>> + Send + '_>> {
        let server_name = server_name.to_owned();
        let config = Arc::clone(&self.config);
        Box::pin(async move {
            let dns_name = ServerName::try_from(server_name)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let tls_conn =
                rustls::ClientConnection::new(config, dns_name).map_err(io::Error::other)?;
            Ok(TlsStream::new(stream, tls_conn))
        })
    }
}

pub struct TlsStream<S> {
    inner: S,
    tls: rustls::ClientConnection,
}

impl<S> TlsStream<S> {
    pub fn new(inner: S, tls: rustls::ClientConnection) -> Self {
        Self { inner, tls }
    }

    pub fn tls_connection(&self) -> &rustls::ClientConnection {
        &self.tls
    }
}

impl<S: Unpin> Unpin for TlsStream<S> {}

impl<S> Read for TlsStream<S>
where
    S: Read + Write + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // First, try to read any buffered plaintext from rustls
        let plaintext_slice = unsafe {
            let uninit = buf.as_mut();
            std::slice::from_raw_parts_mut(uninit.as_mut_ptr() as *mut u8, uninit.len())
        };

        match this.tls.reader().read(plaintext_slice) {
            Ok(n) => {
                unsafe { buf.advance(n) };
                return Poll::Ready(Ok(()));
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Poll::Ready(Err(e)),
        }

        // Feed ciphertext from the underlying stream into rustls
        match read_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(0)) => return Poll::Ready(Ok(())),
            Poll::Ready(Ok(_n)) => {
                this.tls
                    .process_new_packets()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            }
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }

        // Try reading plaintext again after processing
        match this.tls.reader().read(plaintext_slice) {
            Ok(n) => {
                unsafe { buf.advance(n) };
                Poll::Ready(Ok(()))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl<S> Write for TlsStream<S>
where
    S: Read + Write + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();

        // Write plaintext into rustls
        let n = match this.tls.writer().write(buf) {
            Ok(n) => n,
            Err(e) => return Poll::Ready(Err(e)),
        };

        // Flush ciphertext to the underlying stream
        match write_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Ready(Ok(n)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // Flush any remaining ciphertext
        match write_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(_)) => {
                // Also flush the underlying stream
                Pin::new(&mut this.inner).poll_flush(cx)
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.tls.send_close_notify();

        // Flush the close_notify
        match write_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(_)) => Pin::new(&mut this.inner).poll_shutdown(cx),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Read ciphertext from the async stream into rustls.
fn read_tls<S: Read + Unpin>(
    tls: &mut rustls::ClientConnection,
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<io::Result<usize>> {
    struct AsyncReader<'a, 'b, S> {
        stream: &'a mut S,
        cx: &'a mut Context<'b>,
    }

    impl<S: Read + Unpin> StdRead for AsyncReader<'_, '_, S> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut read_buf = rt::ReadBuf::new(buf);
            match Pin::new(&mut *self.stream).poll_read(self.cx, read_buf.unfilled()) {
                Poll::Ready(Ok(())) => Ok(read_buf.filled().len()),
                Poll::Ready(Err(e)) => Err(e),
                Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }
    }

    let mut reader = AsyncReader { stream, cx };
    match tls.read_tls(&mut reader) {
        Ok(n) => Poll::Ready(Ok(n)),
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
        Err(e) => Poll::Ready(Err(e)),
    }
}

/// Write ciphertext from rustls to the async stream.
fn write_tls<S: Write + Unpin>(
    tls: &mut rustls::ClientConnection,
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<io::Result<usize>> {
    struct AsyncWriter<'a, 'b, S> {
        stream: &'a mut S,
        cx: &'a mut Context<'b>,
    }

    impl<S: Write + Unpin> StdWrite for AsyncWriter<'_, '_, S> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match Pin::new(&mut *self.stream).poll_write(self.cx, buf) {
                Poll::Ready(r) => r,
                Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            match Pin::new(&mut *self.stream).poll_flush(self.cx) {
                Poll::Ready(r) => r,
                Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }
    }

    let mut writer = AsyncWriter { stream, cx };
    match tls.write_tls(&mut writer) {
        Ok(n) => Poll::Ready(Ok(n)),
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
        Err(e) => Poll::Ready(Err(e)),
    }
}
