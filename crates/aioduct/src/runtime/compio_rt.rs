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

    fn set_tcp_keepalive(
        stream: &Self::TcpStream,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(stream.inner().get_ref());
        let mut keepalive = socket2::TcpKeepalive::new().with_time(time);
        if let Some(interval) = interval {
            keepalive = keepalive.with_interval(interval);
        }
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
        ))]
        if let Some(retries) = retries {
            keepalive = keepalive.with_retries(retries);
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
        )))]
        let _ = retries;
        sock_ref.set_tcp_keepalive(&keepalive)
    }

    #[cfg(target_os = "linux")]
    fn bind_device(stream: &Self::TcpStream, interface: &str) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(stream.inner().get_ref());
        sock_ref.bind_device(Some(interface.as_bytes()))
    }

    fn from_std_tcp(stream: std::net::TcpStream) -> io::Result<Self::TcpStream> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let async_stream = async_io::Async::new(stream)?;
        Ok(CompioIo::new(async_stream))
    }

    fn connect_bound(
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
        AssertSend(async move {
            use socket2::{Domain, Protocol, SockAddr, Socket, Type};

            let std_stream = compio_runtime::spawn_blocking(move || {
                let domain = if addr.is_ipv4() {
                    Domain::IPV4
                } else {
                    Domain::IPV6
                };
                let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
                socket.bind(&SockAddr::from(std::net::SocketAddr::new(local, 0)))?;
                socket.connect(&SockAddr::from(addr))?;
                socket.set_nodelay(true)?;
                Ok::<std::net::TcpStream, io::Error>(socket.into())
            })
            .await
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
            let std_stream = std_stream?;
            std_stream.set_nonblocking(true)?;
            let async_stream = async_io::Async::new(std_stream)?;
            Ok(CompioIo::new(async_stream))
        })
    }

    #[cfg(unix)]
    type UnixStream = CompioIo<async_io::Async<std::os::unix::net::UnixStream>>;

    #[cfg(unix)]
    fn connect_unix(
        path: &std::path::Path,
    ) -> impl Future<Output = io::Result<Self::UnixStream>> + Send {
        let path = path.to_owned();
        AssertSend(async move {
            let stream = async_io::Async::<std::os::unix::net::UnixStream>::connect(&path).await?;
            Ok(CompioIo::new(stream))
        })
    }
}

/// Compio-backed sleep future.
pub struct CompioSleep {
    inner: Pin<Box<dyn Future<Output = ()>>>,
}

// Safety: see AssertSend rationale above.
unsafe impl Send for CompioSleep {}
unsafe impl Sync for CompioSleep {}

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

    /// Get a reference to the inner I/O type.
    pub fn inner(&self) -> &T {
        &self.inner
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
