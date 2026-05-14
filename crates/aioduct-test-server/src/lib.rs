pub mod counting;
pub mod h1;
pub mod h2;
#[cfg(feature = "http3")]
pub mod h3;
pub mod raw;
pub mod stale;
#[cfg(feature = "rustls")]
pub mod tls;

pub use counting::ConnectionCounter;

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::rt::{self, Read, Write};

#[derive(Clone)]
pub struct TokioExec;

impl<F> hyper::rt::Executor<F> for TokioExec
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

pin_project_lite::pin_project! {
    pub struct TokioIo<T> {
        #[pin]
        inner: T,
    }
}

impl<T> TokioIo<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T> Read for TokioIo<T>
where
    T: tokio::io::AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let n = unsafe {
            let mut tbuf = tokio::io::ReadBuf::uninit(buf.as_mut());
            match tokio::io::AsyncRead::poll_read(self.project().inner, cx, &mut tbuf) {
                Poll::Ready(Ok(())) => tbuf.filled().len(),
                other => return other,
            }
        };
        unsafe {
            buf.advance(n);
        }
        Poll::Ready(Ok(()))
    }
}

impl<T> Write for TokioIo<T>
where
    T: tokio::io::AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(self.project().inner, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(self.project().inner, cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(self.project().inner, cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write_vectored(self.project().inner, cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        tokio::io::AsyncWrite::is_write_vectored(&self.inner)
    }
}
