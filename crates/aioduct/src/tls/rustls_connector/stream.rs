use std::io;
use std::io::{Read as StdRead, Write as StdWrite};
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::rt::{self, Read, Write};

/// A TLS-wrapped stream implementing hyper's `Read` and `Write`.
pub struct TlsStream<S> {
    pub(super) inner: S,
    pub(super) tls: rustls::ClientConnection,
}

impl<S> TlsStream<S> {
    /// Create a TLS stream wrapping the given transport and connection.
    pub fn new(inner: S, tls: rustls::ClientConnection) -> Self {
        Self { inner, tls }
    }

    /// Get a reference to the underlying rustls connection.
    pub fn tls_connection(&self) -> &rustls::ClientConnection {
        &self.tls
    }

    /// Extract TLS handshake info (peer certificate, etc.).
    pub fn tls_info(&self) -> crate::tls::TlsInfo {
        crate::tls::TlsInfo::from_rustls(&self.tls)
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

        let plaintext_slice = unsafe {
            let uninit = buf.as_mut();
            std::slice::from_raw_parts_mut(uninit.as_mut_ptr() as *mut u8, uninit.len())
        };

        // First, try to read any buffered plaintext from rustls
        match this.tls.reader().read(plaintext_slice) {
            Ok(n) if n > 0 => {
                unsafe { buf.advance(n) };
                return Poll::Ready(Ok(()));
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Poll::Ready(Err(e)),
        }

        // Keep feeding ciphertext until we get plaintext or the inner stream
        // returns Pending. This handles TLS messages (like NewSessionTicket)
        // that produce no plaintext — we must loop back to read_tls so the
        // waker gets properly registered on the inner stream.
        loop {
            match read_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Ok(())),
                Poll::Ready(Ok(_n)) => {
                    this.tls
                        .process_new_packets()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    if this.tls.wants_write()
                        && let Poll::Ready(Err(e)) = write_tls(&mut this.tls, &mut this.inner, cx)
                    {
                        return Poll::Ready(Err(e));
                    }
                    match this.tls.reader().read(plaintext_slice) {
                        Ok(n) if n > 0 => {
                            unsafe { buf.advance(n) };
                            return Poll::Ready(Ok(()));
                        }
                        Ok(_) => {}
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => return Poll::Ready(Err(e)),
                    }
                    // No plaintext yet (e.g. NewSessionTicket), loop to read more ciphertext
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
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

        // Drain all ciphertext from rustls to the underlying stream
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => break,
            }
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // Drain all remaining ciphertext from rustls to the underlying stream
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        // Also flush the underlying stream
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.tls.send_close_notify();

        // Drain the close_notify
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// Read ciphertext from the async stream into rustls.
pub(super) fn read_tls<S: Read + Unpin>(
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
pub(super) fn write_tls<S: Write + Unpin>(
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
