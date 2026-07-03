use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::{self, Read, Write};
use pin_project_lite::pin_project;

use super::{ConnectorLocal, RuntimeCompletion, RuntimeLocal};

/// Compio async runtime implementation using native io_uring/IOCP for TCP I/O.
pub struct CompioRuntime;

// ── New trait impls (v0.2) ──────────────────────────────────────────────────

impl RuntimeCompletion for CompioRuntime {
    type Sleep = CompioSleep;

    fn sleep(duration: Duration) -> Self::Sleep {
        CompioSleep::new(async_io::Timer::after(duration))
    }

    fn block_on<F: Future>(future: F) -> Result<F::Output, crate::error::Error> {
        let rt = compio_runtime::Runtime::new().map_err(crate::error::Error::Io)?;
        Ok(rt.block_on(future))
    }
}

impl RuntimeLocal for CompioRuntime {
    fn spawn_local<F: Future<Output = ()> + 'static>(future: F) {
        compio_runtime::spawn(future).detach();
    }
}

// CompioRuntime does NOT implement RuntimePoll — it's completion-based,
// single-threaded, and cannot migrate futures between threads.

// ── SocketConfig ──────────────────────────────────────────────────────────

impl super::SocketConfig for CompioTcpStream {
    fn set_keepalive(
        &self,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        use socket2::SockRef;
        let sock_ref = SockRef::from(&self.socket_handle);
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

        let fd = self.socket_handle.as_raw_fd();
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
        let sock_ref = SockRef::from(&self.socket_handle);
        sock_ref.bind_device(Some(interface.as_bytes()))
    }
}

// ── TcpConnector ──────────────────────────────────────────────────────────

/// TCP connector for the Compio runtime.
///
/// Uses native `compio_net::TcpStream` for io_uring (Linux) / IOCP (Windows)
/// I/O, bridged to futures-io via `compio_io::compat::AsyncStream`.
#[derive(Clone, Copy, Default)]
pub struct TcpConnector;

impl ConnectorLocal for TcpConnector {
    type Stream = CompioTcpStream;

    async fn connect(&self, addr: SocketAddr) -> io::Result<Self::Stream> {
        let stream = compio_net::TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(CompioTcpStream::new(stream))
    }

    async fn connect_bound(
        &self,
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> io::Result<Self::Stream> {
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
            socket.set_tcp_nodelay(true)?;
            Ok::<std::net::TcpStream, io::Error>(socket.into())
        })
        .await
        .map_err(|e| io::Error::other(format!("{e:?}")))?;
        let std_stream = std_stream?;
        std_stream.set_nonblocking(true)?;
        let compio_stream = compio_net::TcpStream::from_std(std_stream)?;
        Ok(CompioTcpStream::new(compio_stream))
    }

    fn from_std_tcp(&self, stream: std::net::TcpStream) -> io::Result<Self::Stream> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        let compio_stream = compio_net::TcpStream::from_std(stream)?;
        Ok(CompioTcpStream::new(compio_stream))
    }

    fn into_std_tcp(&self, stream: Self::Stream) -> io::Result<std::net::TcpStream> {
        use socket2::SockRef;
        let sock = SockRef::from(&stream.socket_handle).try_clone()?;
        drop(stream);
        let std_stream: std::net::TcpStream = sock.into();
        std_stream.set_nonblocking(false)?;
        Ok(std_stream)
    }
}

// ── CompioTcpStream ──────────────────────────────────────────────────────────

pin_project! {
    /// Compound stream type that keeps a `compio_net::TcpStream` handle alongside
    /// the async I/O bridge for socket operations (keepalive, fast open, etc.).
    ///
    /// `compio_net::TcpStream` is clone-cheap (shared fd), so this is essentially
    /// free. The `socket_handle` is used by `set_tcp_keepalive` etc. to access the
    /// raw socket via `AsFd`.
    ///
    pub struct CompioTcpStream {
        io: Pin<Box<CompioIo<compio_io::compat::AsyncStream<compio_net::TcpStream>>>>,
        pub(crate) socket_handle: compio_net::TcpStream,
    }
}

impl CompioTcpStream {
    pub(crate) fn new(stream: compio_net::TcpStream) -> Self {
        let socket_handle = stream.clone();
        Self {
            io: Box::pin(CompioIo::new(compio_io::compat::AsyncStream::new(stream))),
            socket_handle,
        }
    }
}

impl Read for CompioTcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Read::poll_read(self.project().io.as_mut(), cx, buf)
    }
}

impl Write for CompioTcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Write::poll_write(self.project().io.as_mut(), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Write::poll_flush(self.project().io.as_mut(), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Write::poll_shutdown(self.project().io.as_mut(), cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Write::poll_write_vectored(self.project().io.as_mut(), cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

pin_project! {
    /// Compio-backed sleep future using async_io::Timer.
    ///
    /// Uses async_io's timer because compio_runtime's `TimerFuture` is `!Send`.
    pub struct CompioSleep {
        #[pin]
        inner: async_io::Timer,
    }
}

impl CompioSleep {
    pub(crate) fn new(inner: async_io::Timer) -> Self {
        Self { inner }
    }
}

impl Future for CompioSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().inner.poll(cx) {
            Poll::Ready(_instant) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
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

// ── DefaultResolver ───────────────────────────────────────────────────────

/// Default DNS resolver for compio using blocking `getaddrinfo` via
/// `compio_runtime::spawn_blocking`.
pub struct DefaultResolver;

impl super::Resolve for DefaultResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        let addr = format!("{host}:{port}");
        Box::pin(AssertSend(async move {
            let addrs = compio_runtime::spawn_blocking(move || {
                use std::net::ToSocketAddrs;
                addr.to_socket_addrs().map(|iter| iter.collect::<Vec<_>>())
            })
            .await
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
            let addrs = addrs?;
            addrs.into_iter().next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses found")
            })
        }))
    }

    fn resolve_all(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
        let addr = format!("{host}:{port}");
        Box::pin(AssertSend(async move {
            let addrs = compio_runtime::spawn_blocking(move || {
                use std::net::ToSocketAddrs;
                addr.to_socket_addrs().map(|iter| iter.collect::<Vec<_>>())
            })
            .await
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
            let addrs = addrs?;
            if addrs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "no addresses found",
                ));
            }
            Ok(addrs)
        }))
    }
}

/// Wrapper that unsafely implements `Send` for a `!Send` future.
///
/// # Safety
///
/// Only safe in compio's thread-per-core model where futures never cross
/// thread boundaries.
struct AssertSend<F>(F);

unsafe impl<F> Send for AssertSend<F> {}

impl<F: Future> Future for AssertSend<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        inner.poll(cx)
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn connector_connect_works() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let stream = connector.connect(addr).await.unwrap();
            assert!(Write::is_write_vectored(&stream));
        });
    }

    // ── New trait tests (v0.2) ──────────────────────────────────────────────

    #[test]
    fn runtime_completion_sleep() {
        use crate::runtime::RuntimeCompletion;
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let start = std::time::Instant::now();
            <CompioRuntime as RuntimeCompletion>::sleep(Duration::from_millis(10)).await;
            assert!(start.elapsed() >= Duration::from_millis(10));
        });
    }

    #[test]
    fn runtime_local_spawn_local() {
        use crate::runtime::RuntimeLocal;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let flag = Arc::new(AtomicBool::new(false));
            let flag2 = flag.clone();
            CompioRuntime::spawn_local(async move {
                flag2.store(true, Ordering::SeqCst);
            });
            compio_runtime::time::sleep(Duration::from_millis(10)).await;
            assert!(flag.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn connector_connect_bound_works() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let local: std::net::IpAddr = "127.0.0.1".parse().unwrap();
            let stream =
                crate::runtime::ConnectorLocal::connect_bound(&connector, addr, local).await;
            assert!(stream.is_ok());
        });
    }

    #[test]
    fn connector_from_std_tcp_works() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let std_stream = std::net::TcpStream::connect(addr).unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let result = crate::runtime::ConnectorLocal::from_std_tcp(&connector, std_stream);
            assert!(result.is_ok());
        });
    }

    // ── ConnectorLocal connect_bound IPv6 path ─────────────────────────────

    #[test]
    fn connector_connect_bound_ipv6() {
        let listener = match std::net::TcpListener::bind("[::1]:0") {
            Ok(l) => l,
            Err(_) => return, // Skip if IPv6 not available
        };
        let addr = listener.local_addr().unwrap();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let connector = TcpConnector;
            let local: std::net::IpAddr = "::1".parse().unwrap();
            let stream =
                crate::runtime::ConnectorLocal::connect_bound(&connector, addr, local).await;
            assert!(stream.is_ok());
        });
    }

    // ── DefaultResolver resolve_all error ─────────────────────────────

    #[test]
    fn default_resolver_resolve_all_invalid_host_errors() {
        use crate::runtime::Resolve;
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resolver = DefaultResolver;
            let result = resolver
                .resolve_all("this.host.does.not.exist.invalid", 80)
                .await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn default_resolver_resolve_single() {
        use crate::runtime::Resolve;
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resolver = DefaultResolver;
            let addr = resolver.resolve("localhost", 80).await.unwrap();
            assert_eq!(addr.port(), 80);
        });
    }

    #[test]
    fn block_on_works() {
        use crate::runtime::RuntimeCompletion;
        let result = CompioRuntime::block_on(async { 42 }).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn compio_io_inner_accessor() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let io = CompioIo::new(
            async_io::Async::<std::net::TcpStream>::try_from(
                std::net::TcpStream::connect(addr).unwrap(),
            )
            .unwrap(),
        );
        let _inner = io.inner();
    }

    // ── CompioIo tests ─────────────────────────────────────────────────

    #[test]
    fn compio_io_new_and_inner() {
        let val = 42u32;
        let io = CompioIo::new(val);
        assert_eq!(*io.inner(), 42u32);
    }

    // ── DefaultResolver resolve_all ────────────────────────────────────

    #[test]
    fn default_resolver_resolve_all_multiple() {
        use crate::runtime::Resolve;
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resolver = DefaultResolver;
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
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resolver = DefaultResolver;
            let result = resolver
                .resolve("this.host.does.not.exist.invalid", 80)
                .await;
            assert!(result.is_err());
        });
    }

    // ── CompioSleep new() constructor ──────────────────────────────────

    #[test]
    fn compio_sleep_new_completes() {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let timer = async_io::Timer::after(Duration::from_millis(5));
            let sleep = CompioSleep::new(timer);
            let start = std::time::Instant::now();
            sleep.await;
            assert!(start.elapsed() >= Duration::from_millis(5));
        });
    }

    // ── RuntimeLocal ───────────────────────────────────────────────────

    #[test]
    fn runtime_completion_block_on_nested() {
        use crate::runtime::RuntimeCompletion;
        // block_on should work for simple computations
        let result = CompioRuntime::block_on(async { "hello".len() }).unwrap();
        assert_eq!(result, 5);
    }
}
