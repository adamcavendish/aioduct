use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{self, Read, Write};
use pin_project_lite::pin_project;

use super::Runtime;

/// Smol async runtime implementation.
pub struct SmolRuntime;

impl Runtime for SmolRuntime {
    type TcpStream = SmolIo<smol::net::TcpStream>;
    type Sleep = SmolSleep;

    async fn connect(addr: SocketAddr) -> io::Result<Self::TcpStream> {
        let stream = smol::net::TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(SmolIo::new(stream))
    }

    async fn resolve(host: &str, port: u16) -> io::Result<SocketAddr> {
        let addr = format!("{host}:{port}");
        smol::net::resolve(addr)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses found"))
    }

    fn sleep(duration: Duration) -> Self::Sleep {
        SmolSleep {
            inner: async_io::Timer::after(duration),
        }
    }

    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        smol::spawn(future).detach();
    }
}

// -- SmolSleep --

pin_project! {
    /// Smol-backed sleep future.
    pub struct SmolSleep {
        #[pin]
        inner: async_io::Timer,
    }
}

impl Future for SmolSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().inner.poll(cx) {
            Poll::Ready(_instant) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

// -- SmolIo: bridges futures-io AsyncRead/AsyncWrite to hyper::rt::Read/Write --

pin_project! {
    /// Adapter bridging futures-io `AsyncRead`/`AsyncWrite` to hyper's `Read`/`Write`.
    pub struct SmolIo<T> {
        #[pin]
        inner: T,
    }
}

impl<T> SmolIo<T> {
    /// Wrap a futures-io type.
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    /// Get a reference to the inner I/O type.
    pub fn inner(&self) -> &T {
        &self.inner
    }
}

impl<T> Read for SmolIo<T>
where
    T: futures_io::AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let slice = unsafe {
            let uninit = buf.as_mut();
            // Zero-initialize for safety with futures-io which expects &mut [u8]
            std::ptr::write_bytes(uninit.as_mut_ptr(), 0, uninit.len());
            std::slice::from_raw_parts_mut(uninit.as_mut_ptr() as *mut u8, uninit.len())
        };
        match futures_io::AsyncRead::poll_read(self.project().inner, cx, slice) {
            Poll::Ready(Ok(n)) => {
                unsafe { buf.advance(n) };
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Write for SmolIo<T>
where
    T: futures_io::AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        futures_io::AsyncWrite::poll_write(self.project().inner, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        futures_io::AsyncWrite::poll_flush(self.project().inner, cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        futures_io::AsyncWrite::poll_close(self.project().inner, cx)
    }
}
