use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{self, Read, Write};
use pin_project_lite::pin_project;

use super::{Connector, RuntimeCompletion, RuntimePoll};

/// Smol async runtime implementation.
pub struct SmolRuntime;

// ── New trait impls (v0.2) ──────────────────────────────────────────────────

impl RuntimeCompletion for SmolRuntime {
    type Sleep = SmolSleep;

    fn sleep(duration: Duration) -> Self::Sleep {
        SmolSleep {
            inner: async_io::Timer::after(duration),
        }
    }

    fn block_on<F: Future>(future: F) -> Result<F::Output, crate::error::Error> {
        Ok(smol::block_on(future))
    }
}

impl RuntimePoll for SmolRuntime {
    fn spawn_send<F: Future<Output = ()> + Send + 'static>(future: F) {
        smol::spawn(future).detach();
    }
}

// ── SocketConfig ──────────────────────────────────────────────────────────

impl super::SocketConfig for SmolIo<smol::net::TcpStream> {
    fn set_keepalive(
        &self,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(self.inner());
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
    fn set_fast_open(&self) -> io::Result<()> {
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

        let sock_ref = SockRef::from(self.inner());
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
    fn bind_device(&self, interface: &str) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(self.inner());
        sock_ref.bind_device(Some(interface.as_bytes()))
    }
}

// ── TcpConnector ──────────────────────────────────────────────────────────

/// TCP connector for the Smol runtime.
#[derive(Clone, Copy, Default)]
pub struct TcpConnector;

impl Connector for TcpConnector {
    type Stream = SmolIo<smol::net::TcpStream>;

    async fn connect(&self, addr: SocketAddr) -> io::Result<Self::Stream> {
        let stream = smol::net::TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(SmolIo::new(stream))
    }

    async fn connect_bound(
        &self,
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> io::Result<Self::Stream> {
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

    fn from_std_tcp(&self, stream: std::net::TcpStream) -> io::Result<Self::Stream> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let async_stream = smol::net::TcpStream::try_from(stream)?;
        Ok(SmolIo::new(async_stream))
    }
}

#[allow(clippy::manual_async_fn)]
impl super::ConnectorSend for TcpConnector {
    type Stream = SmolIo<smol::net::TcpStream>;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        async move {
            let stream = smol::net::TcpStream::connect(addr).await?;
            stream.set_nodelay(true)?;
            Ok(SmolIo::new(stream))
        }
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        async move {
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
    }

    fn from_std_tcp(&self, stream: std::net::TcpStream) -> io::Result<Self::Stream> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let async_stream = smol::net::TcpStream::try_from(stream)?;
        Ok(SmolIo::new(async_stream))
    }
}

// ── DefaultResolver ───────────────────────────────────────────────────────

/// Default DNS resolver using smol's `net::resolve`.
pub struct DefaultResolver;

impl super::Resolve for DefaultResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        let addr = format!("{host}:{port}");
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = smol::net::resolve(addr).await?;
            addrs.into_iter().next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses found")
            })
        })
    }

    fn resolve_all(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
        let addr = format!("{host}:{port}");
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = smol::net::resolve(addr).await?;
            if addrs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "no addresses found",
                ));
            }
            Ok(addrs)
        })
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

impl SmolSleep {
    /// Create a new sleep future from an async-io timer.
    pub(crate) fn new(inner: async_io::Timer) -> Self {
        Self { inner }
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
#[allow(deprecated)]
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
            <SmolRuntime as Runtime>::sleep(Duration::from_millis(10)).await;
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

    // ── New trait tests (v0.2) ──────────────────────────────────────────────

    #[test]
    fn runtime_completion_sleep() {
        use crate::runtime::RuntimeCompletion;
        smol::block_on(async {
            let start = std::time::Instant::now();
            <SmolRuntime as RuntimeCompletion>::sleep(Duration::from_millis(10)).await;
            assert!(start.elapsed() >= Duration::from_millis(10));
        });
    }

    #[test]
    fn runtime_poll_spawn_send() {
        use crate::runtime::RuntimePoll;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        smol::block_on(async {
            let flag = Arc::new(AtomicBool::new(false));
            let flag2 = flag.clone();
            SmolRuntime::spawn_send(async move {
                flag2.store(true, Ordering::SeqCst);
            });
            // smol needs a timer yield to run detached tasks
            async_io::Timer::after(Duration::from_millis(10)).await;
            assert!(flag.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn connector_connect_works() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let connector = super::TcpConnector;
            let stream = connector.connect(addr).await.unwrap();
            assert!(Write::is_write_vectored(&stream));
        });
    }

    #[test]
    fn connector_send_connect_bound_works() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let connector = super::TcpConnector;
            let local: std::net::IpAddr = "127.0.0.1".parse().unwrap();
            let stream =
                crate::runtime::ConnectorSend::connect_bound(&connector, addr, local).await;
            assert!(stream.is_ok());
        });
    }

    #[test]
    fn connector_send_from_std_tcp_works() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let std_stream = std::net::TcpStream::connect(addr).unwrap();
            let connector = super::TcpConnector;
            let result = crate::runtime::ConnectorSend::from_std_tcp(&connector, std_stream);
            assert!(result.is_ok());
        });
    }

    #[test]
    fn default_resolver_resolve_single() {
        use crate::runtime::Resolve;
        smol::block_on(async {
            let resolver = super::DefaultResolver;
            let addr = resolver.resolve("localhost", 80).await.unwrap();
            assert_eq!(addr.port(), 80);
        });
    }

    #[test]
    fn block_on_works() {
        use crate::runtime::RuntimeCompletion;
        let result = SmolRuntime::block_on(async { 42 }).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn set_keepalive_interval_none() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let stream = SmolRuntime::connect(addr).await.unwrap();
            let result =
                SmolRuntime::set_tcp_keepalive(&stream, Duration::from_secs(60), None, None);
            assert!(result.is_ok());
        });
    }

    #[test]
    fn connector_local_connect_bound_works() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let connector = super::TcpConnector;
            let local: std::net::IpAddr = "127.0.0.1".parse().unwrap();
            let stream = crate::runtime::Connector::connect_bound(&connector, addr, local).await;
            assert!(stream.is_ok());
        });
    }

    #[test]
    fn connector_local_from_std_tcp_works() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let std_stream = std::net::TcpStream::connect(addr).unwrap();
            let connector = super::TcpConnector;
            let result = crate::runtime::Connector::from_std_tcp(&connector, std_stream);
            assert!(result.is_ok());
        });
    }

    // ── Connector connect_bound IPv6 path ─────────────────────────────

    #[test]
    fn connector_local_connect_bound_ipv6() {
        smol::block_on(async {
            let listener = match smol::net::TcpListener::bind("[::1]:0").await {
                Ok(l) => l,
                Err(_) => return, // Skip if IPv6 not available
            };
            let addr = listener.local_addr().unwrap();
            let connector = super::TcpConnector;
            let local: std::net::IpAddr = "::1".parse().unwrap();
            let stream = crate::runtime::Connector::connect_bound(&connector, addr, local).await;
            assert!(stream.is_ok());
        });
    }

    #[test]
    fn connector_send_connect_bound_ipv6() {
        smol::block_on(async {
            let listener = match smol::net::TcpListener::bind("[::1]:0").await {
                Ok(l) => l,
                Err(_) => return, // Skip if IPv6 not available
            };
            let addr = listener.local_addr().unwrap();
            let connector = super::TcpConnector;
            let local: std::net::IpAddr = "::1".parse().unwrap();
            let stream =
                crate::runtime::ConnectorSend::connect_bound(&connector, addr, local).await;
            assert!(stream.is_ok());
        });
    }

    // ── DefaultResolver resolve_all error ─────────────────────────────

    #[test]
    fn default_resolver_resolve_all_invalid_host_errors() {
        use crate::runtime::Resolve;
        smol::block_on(async {
            let resolver = super::DefaultResolver;
            let result = resolver
                .resolve_all("this.host.does.not.exist.invalid", 80)
                .await;
            assert!(result.is_err());
        });
    }

    // ── SmolIo read/write edge cases ───────────────────────────────────

    #[test]
    fn smol_io_read_eof_returns_zero_advance() {
        use std::future::poll_fn;

        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let stream = smol::net::TcpStream::connect(addr).await.unwrap();
            let (server, _) = listener.accept().await.unwrap();
            drop(server); // close write end

            let mut io = SmolIo::new(stream);
            let mut buf = [0u8; 64];
            let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);

            poll_fn(|cx| Pin::new(&mut io).poll_read(cx, read_buf.unfilled()))
                .await
                .unwrap();
            assert_eq!(
                read_buf.filled().len(),
                0,
                "EOF should produce 0 filled bytes"
            );
        });
    }

    #[test]
    fn smol_io_write_and_flush() {
        use std::future::poll_fn;

        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let stream = smol::net::TcpStream::connect(addr).await.unwrap();
            let (mut server, _) = listener.accept().await.unwrap();

            let mut io = SmolIo::new(stream);

            let data = b"smol io test";
            let n = poll_fn(|cx| Pin::new(&mut io).poll_write(cx, data))
                .await
                .unwrap();
            assert_eq!(n, data.len());

            poll_fn(|cx| Pin::new(&mut io).poll_flush(cx))
                .await
                .unwrap();

            // Read from the other end
            use futures_io::AsyncRead;
            let mut buf = vec![0u8; data.len()];
            let mut read = 0;
            while read < data.len() {
                let n = poll_fn(|cx| Pin::new(&mut server).poll_read(cx, &mut buf[read..]))
                    .await
                    .unwrap();
                read += n;
            }
            assert_eq!(&buf, data);
        });
    }

    #[test]
    fn smol_io_shutdown_closes_write() {
        use std::future::poll_fn;

        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let stream = smol::net::TcpStream::connect(addr).await.unwrap();
            let (mut server, _) = listener.accept().await.unwrap();

            let mut io = SmolIo::new(stream);

            poll_fn(|cx| Pin::new(&mut io).poll_shutdown(cx))
                .await
                .unwrap();

            // Reader should see EOF
            use futures_io::AsyncRead;
            let mut buf = [0u8; 1];
            let n = poll_fn(|cx| Pin::new(&mut server).poll_read(cx, &mut buf))
                .await
                .unwrap();
            assert_eq!(n, 0);
        });
    }

    #[test]
    fn smol_io_inner_accessor() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let stream = smol::net::TcpStream::connect(addr).await.unwrap();
            let io = SmolIo::new(stream);
            // inner() returns a reference to the TcpStream
            let peer = io.inner().peer_addr();
            assert!(peer.is_ok());
        });
    }

    // ── DefaultResolver resolve_all ────────────────────────────────────

    #[test]
    fn default_resolver_resolve_all_multiple() {
        use crate::runtime::Resolve;
        smol::block_on(async {
            let resolver = super::DefaultResolver;
            let addrs = resolver.resolve_all("localhost", 80).await.unwrap();
            assert!(!addrs.is_empty());
            for addr in &addrs {
                assert_eq!(addr.port(), 80);
            }
        });
    }

    #[test]
    fn default_resolver_invalid_host_errors() {
        use crate::runtime::Resolve;
        smol::block_on(async {
            let resolver = super::DefaultResolver;
            let result = resolver
                .resolve("this.host.does.not.exist.invalid", 80)
                .await;
            assert!(result.is_err());
        });
    }

    // ── SmolSleep new() constructor ────────────────────────────────────

    #[test]
    fn smol_sleep_new_completes() {
        smol::block_on(async {
            let timer = async_io::Timer::after(Duration::from_millis(5));
            let sleep = SmolSleep::new(timer);
            let start = std::time::Instant::now();
            sleep.await;
            assert!(start.elapsed() >= Duration::from_millis(5));
        });
    }
}
