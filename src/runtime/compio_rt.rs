use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{self, Read, Write};
use pin_project_lite::pin_project;

use super::Runtime;

/// Wrapper that unsafely implements Send for a !Send future.
///
/// # Safety
///
/// This is only safe in compio's thread-per-core model where futures are never
/// sent between threads. The CompioRuntime must only be used within a single
/// compio runtime thread.
struct AssertSend<F>(F);

// Safety: compio is thread-per-core — these futures never cross thread boundaries.
unsafe impl<F> Send for AssertSend<F> {}

impl<F: Future> Future for AssertSend<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        inner.poll(cx)
    }
}

/// Compio async runtime implementation using async-io for TCP I/O.
pub struct CompioRuntime;

impl Runtime for CompioRuntime {
    type TcpStream = CompioIo<async_io::Async<std::net::TcpStream>>;
    type Sleep = CompioSleep;

    fn connect(addr: SocketAddr) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
        AssertSend(async move {
            let stream = async_io::Async::<std::net::TcpStream>::connect(addr).await?;
            stream.get_ref().set_nodelay(true)?;
            Ok(CompioIo::new(stream))
        })
    }

    fn resolve(host: &str, port: u16) -> impl Future<Output = io::Result<SocketAddr>> + Send {
        let addr_str = format!("{host}:{port}");
        AssertSend(async move {
            let addrs = compio_runtime::spawn_blocking(move || {
                use std::net::ToSocketAddrs;
                addr_str
                    .to_socket_addrs()
                    .map(|iter| iter.collect::<Vec<_>>())
            })
            .await
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
            let addrs = addrs?;
            addrs.into_iter().next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses found")
            })
        })
    }

    fn sleep(duration: Duration) -> Self::Sleep {
        CompioSleep {
            inner: Box::pin(compio_runtime::time::sleep(duration)),
        }
    }

    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        compio_runtime::spawn(future).detach();
    }
}

/// Compio-backed sleep future.
pub struct CompioSleep {
    inner: Pin<Box<dyn Future<Output = ()>>>,
}

// Safety: see AssertSend rationale above.
unsafe impl Send for CompioSleep {}

impl Future for CompioSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}

pin_project! {
    /// Adapter bridging futures-io `AsyncRead`/`AsyncWrite` to hyper's `Read`/`Write` for compio.
    pub struct CompioIo<T> {
        #[pin]
        inner: T,
    }
}

impl<T> CompioIo<T> {
    /// Wrap an async-io type.
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

// Safety: see AssertSend rationale above.
unsafe impl<T> Send for CompioIo<T> {}

impl<T> Read for CompioIo<T>
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

impl<T> Write for CompioIo<T>
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
