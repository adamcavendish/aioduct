use std::io;
use std::io::{Read as StdRead, Write as StdWrite};
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::rt::{self, Read, Write};

const INTERRUPTED_RETRY_LIMIT: usize = 16;

fn should_retry_interrupted(retries: &mut usize) -> bool {
    *retries += 1;
    *retries < INTERRUPTED_RETRY_LIMIT
}

pub(super) fn poll_flush_retry<S: Write + Unpin>(
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    let mut interrupted_retries = 0;
    loop {
        match Pin::new(&mut *stream).poll_flush(cx) {
            Poll::Ready(Err(e))
                if e.kind() == io::ErrorKind::Interrupted
                    && should_retry_interrupted(&mut interrupted_retries) => {}
            result => return result,
        }
    }
}

fn poll_shutdown_retry<S: Write + Unpin>(
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    let mut interrupted_retries = 0;
    loop {
        match Pin::new(&mut *stream).poll_shutdown(cx) {
            Poll::Ready(Err(e))
                if e.kind() == io::ErrorKind::Interrupted
                    && should_retry_interrupted(&mut interrupted_retries) => {}
            result => return result,
        }
    }
}

/// A TLS-wrapped stream implementing hyper's `Read` and `Write`.
pub struct TlsStream<S> {
    pub(super) inner: S,
    pub(super) tls: rustls::ClientConnection,
    pending_write_error: Option<io::Error>,
    write_shutdown: bool,
}

impl<S> TlsStream<S> {
    /// Create a TLS stream wrapping the given transport and connection.
    pub fn new(inner: S, tls: rustls::ClientConnection) -> Self {
        Self {
            inner,
            tls,
            pending_write_error: None,
            write_shutdown: false,
        }
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
                Poll::Ready(Ok(0)) => match this.tls.reader().read(plaintext_slice) {
                    Ok(n) => {
                        unsafe { buf.advance(n) };
                        return Poll::Ready(Ok(()));
                    }
                    Err(e) => return Poll::Ready(Err(e)),
                },
                Poll::Ready(Ok(_n)) => {
                    this.tls
                        .process_new_packets()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    if this.tls.wants_write() {
                        match write_tls(&mut this.tls, &mut this.inner, cx) {
                            Poll::Ready(Ok(_)) | Poll::Pending => {}
                            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        }
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
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let this = self.get_mut();
        if this.write_shutdown {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "TLS write side is shut down",
            )));
        }
        if let Some(e) = this.pending_write_error.take() {
            return Poll::Ready(Err(e));
        }

        loop {
            while this.tls.wants_write() {
                match write_tls(&mut this.tls, &mut this.inner, cx) {
                    Poll::Ready(Ok(_)) => {}
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                }
            }

            // Writing can queue a TLS control record without accepting
            // application data. Drain it before retrying the plaintext.
            let n = this.tls.writer().write(buf)?;
            if n == 0 {
                if this.tls.wants_write() {
                    continue;
                }
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }

            // Plaintext is accepted at this point, so any transport failure
            // must be reported by a later operation rather than replacing n.
            while this.tls.wants_write() {
                match write_tls(&mut this.tls, &mut this.inner, cx) {
                    Poll::Ready(Ok(_)) => {}
                    Poll::Ready(Err(e)) => {
                        this.pending_write_error = Some(e);
                        return Poll::Ready(Ok(n));
                    }
                    Poll::Pending => return Poll::Ready(Ok(n)),
                }
            }
            return Poll::Ready(Ok(n));
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(e) = this.pending_write_error.take() {
            return Poll::Ready(Err(e));
        }

        // Drain all remaining ciphertext from rustls to the underlying stream
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        // Also flush the underlying stream.
        poll_flush_retry(&mut this.inner, cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.write_shutdown = true;
        if let Some(e) = this.pending_write_error.take() {
            return Poll::Ready(Err(e));
        }
        this.tls.send_close_notify();

        // Drain the close_notify
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        poll_shutdown_retry(&mut this.inner, cx)
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
        pending: bool,
    }

    impl<S: Read + Unpin> StdRead for AsyncReader<'_, '_, S> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut interrupted_retries = 0;
            loop {
                let mut read_buf = rt::ReadBuf::new(buf);
                match Pin::new(&mut *self.stream).poll_read(self.cx, read_buf.unfilled()) {
                    Poll::Ready(Ok(())) => return Ok(read_buf.filled().len()),
                    Poll::Ready(Err(e))
                        if e.kind() == io::ErrorKind::Interrupted
                            && should_retry_interrupted(&mut interrupted_retries) => {}
                    Poll::Ready(Err(e)) => return Err(e),
                    Poll::Pending => {
                        self.pending = true;
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                }
            }
        }
    }

    let mut reader = AsyncReader {
        stream,
        cx,
        pending: false,
    };
    match tls.read_tls(&mut reader) {
        Ok(n) => Poll::Ready(Ok(n)),
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock && reader.pending => Poll::Pending,
        Err(e) => Poll::Ready(Err(e)),
    }
}

pub(super) struct AsyncWriter<'a, 'b, S> {
    stream: &'a mut S,
    cx: &'a mut Context<'b>,
    pending: bool,
}

impl<'a, 'b, S> AsyncWriter<'a, 'b, S> {
    pub(super) fn new(stream: &'a mut S, cx: &'a mut Context<'b>) -> Self {
        Self {
            stream,
            cx,
            pending: false,
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending
    }
}

impl<S: Write + Unpin> StdWrite for AsyncWriter<'_, '_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut interrupted_retries = 0;
        loop {
            match Pin::new(&mut *self.stream).poll_write(self.cx, buf) {
                Poll::Ready(Err(e))
                    if e.kind() == io::ErrorKind::Interrupted
                        && should_retry_interrupted(&mut interrupted_retries) => {}
                Poll::Ready(r) => return r,
                Poll::Pending => {
                    self.pending = true;
                    return Err(io::ErrorKind::WouldBlock.into());
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match poll_flush_retry(self.stream, self.cx) {
            Poll::Ready(result) => result,
            Poll::Pending => {
                self.pending = true;
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
    }
}

/// Write ciphertext from rustls to the async stream.
pub(super) fn write_tls<S: Write + Unpin>(
    tls: &mut rustls::ClientConnection,
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<io::Result<usize>> {
    let had_pending_ciphertext = tls.wants_write();

    let mut writer = AsyncWriter::new(stream, cx);
    match tls.write_tls(&mut writer) {
        Ok(0) if had_pending_ciphertext => Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
        Ok(n) => Poll::Ready(Ok(n)),
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock && writer.is_pending() => Poll::Pending,
        Err(e) => Poll::Ready(Err(e)),
    }
}
