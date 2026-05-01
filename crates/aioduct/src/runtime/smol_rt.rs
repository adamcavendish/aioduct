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

    async fn resolve_all(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let addr = format!("{host}:{port}");
        let addrs: Vec<SocketAddr> = smol::net::resolve(addr).await?;
        if addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "no addresses found",
            ));
        }
        Ok(addrs)
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
    fn set_tcp_fast_open(stream: &Self::TcpStream) -> io::Result<()> {
        use socket2::SockRef;
        use std::os::unix::io::AsRawFd;

        unsafe extern "C" {
            fn setsockopt(
                sockfd: std::ffi::c_int,
                level: std::ffi::c_int,
                optname: std::ffi::c_int,
                optval: *const std::ffi::c_void,
                optlen: u32,
            ) -> std::ffi::c_int;
        }

        let sock_ref = SockRef::from(stream.inner());
        let fd = sock_ref.as_raw_fd();
        const IPPROTO_TCP: std::ffi::c_int = 6;
        const TCP_FASTOPEN_CONNECT: std::ffi::c_int = 30;
        let optval: std::ffi::c_int = 1;
        unsafe {
            let ret = setsockopt(
                fd,
                IPPROTO_TCP,
                TCP_FASTOPEN_CONNECT,
                &optval as *const std::ffi::c_int as *const std::ffi::c_void,
                std::mem::size_of::<std::ffi::c_int>() as u32,
            );
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
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
        let async_stream = smol::net::TcpStream::try_from(stream)?;
        Ok(SmolIo::new(async_stream))
    }

    async fn connect_bound(
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> io::Result<Self::TcpStream> {
        use socket2::{Domain, Protocol, SockAddr, Socket, Type};

        let std_stream = smol::unblock(move || {
            let domain = if addr.is_ipv4() {
                Domain::IPV4
            } else {
                Domain::IPV6
            };
            let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
            socket.bind(&SockAddr::from(std::net::SocketAddr::new(local, 0)))?;
            socket.connect(&SockAddr::from(addr))?;
            socket.set_tcp_nodelay(true)?;
            Ok::<std::net::TcpStream, io::Error>(socket.into())
        })
        .await?;
        std_stream.set_nonblocking(true)?;
        let smol_stream = smol::net::TcpStream::try_from(std_stream)?;
        Ok(SmolIo::new(smol_stream))
    }

    #[cfg(unix)]
    type UnixStream = SmolIo<smol::net::unix::UnixStream>;

    #[cfg(unix)]
    async fn connect_unix(path: &std::path::Path) -> io::Result<Self::UnixStream> {
        let stream = smol::net::unix::UnixStream::connect(path).await?;
        Ok(SmolIo::new(stream))
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

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        futures_io::AsyncWrite::poll_write_vectored(self.project().inner, cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;

    #[test]
    fn resolve_all_localhost() {
        smol::block_on(async {
            let addrs = SmolRuntime::resolve_all("localhost", 80).await.unwrap();
            assert!(!addrs.is_empty());
        });
    }

    #[test]
    fn connect_and_set_keepalive() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let stream = SmolRuntime::connect(addr).await.unwrap();
            let result = SmolRuntime::set_tcp_keepalive(
                &stream,
                Duration::from_secs(60),
                Some(Duration::from_secs(10)),
                Some(3),
            );
            assert!(result.is_ok());
        });
    }

    #[test]
    fn from_std_tcp_succeeds() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let std_stream = std::net::TcpStream::connect(addr).unwrap();
            let smol_stream = SmolRuntime::from_std_tcp(std_stream).unwrap();
            assert!(smol_stream.inner().peer_addr().is_ok());
        });
    }

    #[test]
    fn is_write_vectored_returns_true() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let stream = SmolRuntime::connect(addr).await.unwrap();
            assert!(Write::is_write_vectored(&stream));
        });
    }

    #[test]
    fn write_vectored_delivers_data() {
        use std::future::poll_fn;

        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let mut client = SmolRuntime::connect(addr).await.unwrap();
            let (mut server, _) = listener.accept().await.unwrap();

            let bufs = [
                io::IoSlice::new(b"hello"),
                io::IoSlice::new(b" "),
                io::IoSlice::new(b"world"),
            ];
            let n = poll_fn(|cx| Pin::new(&mut client).poll_write_vectored(cx, &bufs))
                .await
                .unwrap();
            assert_eq!(n, 11);

            use futures_io::AsyncRead;
            let mut buf = vec![0u8; 11];
            let mut read = 0;
            while read < 11 {
                let n = poll_fn(|cx| Pin::new(&mut server).poll_read(cx, &mut buf[read..]))
                    .await
                    .unwrap();
                read += n;
            }
            assert_eq!(&buf, b"hello world");
        });
    }

    #[test]
    fn sleep_completes() {
        smol::block_on(async {
            let start = std::time::Instant::now();
            SmolRuntime::sleep(Duration::from_millis(10)).await;
            assert!(start.elapsed() >= Duration::from_millis(10));
        });
    }

    #[cfg(unix)]
    #[test]
    fn connect_unix_succeeds() {
        smol::block_on(async {
            let dir = std::env::temp_dir().join("aioduct_smol_rt_unix_test");
            let _ = std::fs::create_dir_all(&dir);
            let sock_path = dir.join("rt_test.sock");
            let _ = std::fs::remove_file(&sock_path);

            let _listener = smol::net::unix::UnixListener::bind(&sock_path).unwrap();
            let stream = SmolRuntime::connect_unix(&sock_path).await.unwrap();
            drop(stream);

            let _ = std::fs::remove_file(&sock_path);
            let _ = std::fs::remove_dir(&dir);
        });
    }
}
