use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{self, Read, Write};
use pin_project_lite::pin_project;

use super::Runtime;

/// Tokio async runtime implementation.
pub struct TokioRuntime;

impl Runtime for TokioRuntime {
    type TcpStream = TokioIo<tokio::net::TcpStream>;
    type Sleep = TokioSleep;

    async fn connect(addr: SocketAddr) -> io::Result<Self::TcpStream> {
        let stream = tokio::net::TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(TokioIo::new(stream))
    }

    async fn resolve_all(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let addr = format!("{host}:{port}");
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host(addr).await?.collect();
        if addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "no addresses found",
            ));
        }
        Ok(addrs)
    }

    fn sleep(duration: Duration) -> Self::Sleep {
        TokioSleep {
            inner: tokio::time::sleep(duration),
        }
    }

    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future);
    }

    fn set_tcp_keepalive(
        stream: &Self::TcpStream,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(stream.inner());
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
        let sock_ref = SockRef::from(stream.inner());
        sock_ref.bind_device(Some(interface.as_bytes()))
    }

    fn from_std_tcp(stream: std::net::TcpStream) -> io::Result<Self::TcpStream> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let tokio_stream = tokio::net::TcpStream::from_std(stream)?;
        Ok(TokioIo::new(tokio_stream))
    }

    async fn connect_bound(
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> io::Result<Self::TcpStream> {
        let socket = if addr.is_ipv4() {
            tokio::net::TcpSocket::new_v4()?
        } else {
            tokio::net::TcpSocket::new_v6()?
        };
        socket.bind(std::net::SocketAddr::new(local, 0))?;
        let stream = socket.connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(TokioIo::new(stream))
    }

    #[cfg(unix)]
    type UnixStream = TokioIo<tokio::net::UnixStream>;

    #[cfg(unix)]
    async fn connect_unix(path: &std::path::Path) -> io::Result<Self::UnixStream> {
        let stream = tokio::net::UnixStream::connect(path).await?;
        Ok(TokioIo::new(stream))
    }
}

// -- TokioSleep: bridges hyper::rt::Sleep to tokio::time::Sleep --

pin_project! {
    /// Tokio-backed sleep future.
    pub struct TokioSleep {
        #[pin]
        inner: tokio::time::Sleep,
    }
}

impl Future for TokioSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx)
    }
}

// -- TokioIo: bridges tokio::io::{AsyncRead, AsyncWrite} to hyper::rt::{Read, Write} --

pin_project! {
    /// Adapter bridging tokio's `AsyncRead`/`AsyncWrite` to hyper's `Read`/`Write`.
    pub struct TokioIo<T> {
        #[pin]
        inner: T,
    }
}

impl<T> TokioIo<T> {
    /// Wrap a tokio I/O type.
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    /// Get a reference to the inner I/O type.
    pub fn inner(&self) -> &T {
        &self.inner
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
