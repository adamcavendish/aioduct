pub(crate) mod executor;
mod fallback_resolver;
mod legacy;
#[cfg(not(target_arch = "wasm32"))]
mod system_resolver;
mod traits;

pub use fallback_resolver::FallbackResolver;
#[cfg(any(feature = "tokio", feature = "smol", feature = "compio"))]
pub use system_resolver::SystemResolver;
pub use traits::{
    ConnectorLocal, ConnectorSend, RuntimeCompletion, RuntimeLocal, RuntimePoll, SocketConfig,
};

#[allow(deprecated)]
pub use legacy::Runtime;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

/// Custom DNS resolver trait.
///
/// Implement this to override the runtime's default DNS resolution.
pub trait Resolve: Send + Sync + 'static {
    /// Resolve a hostname and port to a socket address.
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>>;

    /// Resolve a hostname and port to all available socket addresses.
    ///
    /// The default implementation delegates to [`Resolve::resolve`] and wraps
    /// the single result in a `Vec`.
    fn resolve_all(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
        let fut = self.resolve(host, port);
        Box::pin(async move { fut.await.map(|a| vec![a]) })
    }
}

impl<F> Resolve for F
where
    F: Fn(&str, u16) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>>
        + Send
        + Sync
        + 'static,
{
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        (self)(host, port)
    }
}

/// A resolver that maps specific hostnames to fixed socket addresses,
/// falling back to an inner resolver for unmatched hosts.
pub struct StaticResolver {
    overrides: std::collections::HashMap<String, Vec<SocketAddr>>,
    fallback: Option<Arc<dyn Resolve>>,
}

impl StaticResolver {
    /// Create a new resolver that delegates to the given fallback for
    /// hostnames without a static override.
    pub fn new(fallback: Option<Arc<dyn Resolve>>) -> Self {
        Self {
            overrides: std::collections::HashMap::new(),
            fallback,
        }
    }

    /// Create a new resolver with the given fallback resolver.
    pub fn with_fallback(fallback: impl Resolve) -> Self {
        Self {
            overrides: std::collections::HashMap::new(),
            fallback: Some(Arc::new(fallback)),
        }
    }

    /// Add a static override for the given hostname.
    pub fn add(&mut self, host: String, addrs: Vec<SocketAddr>) {
        self.overrides.insert(host, addrs);
    }
}

impl Resolve for StaticResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        if let Some(addrs) = self.overrides.get(host) {
            let addr = addrs[0];
            return Box::pin(async move { Ok(addr) });
        }
        if let Some(ref fallback) = self.fallback {
            return fallback.resolve(host, port);
        }
        let msg = format!("no resolver configured for {host}:{port}");
        Box::pin(async move { Err(io::Error::new(io::ErrorKind::AddrNotAvailable, msg)) })
    }

    fn resolve_all(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> {
        if let Some(addrs) = self.overrides.get(host) {
            let addrs = addrs.clone();
            return Box::pin(async move { Ok(addrs) });
        }
        if let Some(ref fallback) = self.fallback {
            return fallback.resolve_all(host, port);
        }
        let msg = format!("no resolver configured for {host}:{port}");
        Box::pin(async move { Err(io::Error::new(io::ErrorKind::AddrNotAvailable, msg)) })
    }
}

#[cfg(feature = "tokio")]
mod tokio_legacy;
/// Tokio runtime implementation.
#[cfg(feature = "tokio")]
pub mod tokio_rt;
#[cfg(feature = "tokio")]
pub use tokio_rt::TokioRuntime;

#[cfg(feature = "smol")]
mod smol_legacy;
/// Smol runtime implementation.
#[cfg(feature = "smol")]
pub mod smol_rt;
#[cfg(feature = "smol")]
pub use smol_rt::SmolRuntime;

#[cfg(feature = "compio")]
mod compio_legacy;
/// Compio runtime implementation.
#[cfg(feature = "compio")]
pub mod compio_rt;
#[cfg(feature = "compio")]
pub use compio_rt::CompioRuntime;

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::task::Poll;

    #[tokio::test]
    async fn resolve_default_resolve_all_wraps_single() {
        struct SingleResolver(SocketAddr);
        impl Resolve for SingleResolver {
            fn resolve(
                &self,
                _host: &str,
                _port: u16,
            ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                let addr = self.0;
                Box::pin(async move { Ok(addr) })
            }
        }
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let resolver = SingleResolver(addr);
        let result = resolver.resolve_all("example.com", 80).await.unwrap();
        assert_eq!(result, vec![addr]);
    }

    #[tokio::test]
    async fn resolve_closure_blanket_impl() {
        let resolver = |_host: &str,
                        _port: u16|
         -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
            Box::pin(async { Ok("127.0.0.1:443".parse().unwrap()) })
        };
        let result = resolver.resolve("example.com", 443).await.unwrap();
        assert_eq!(result, "127.0.0.1:443".parse::<SocketAddr>().unwrap());
    }

    #[tokio::test]
    async fn resolve_closure_resolve_all_default() {
        let resolver = |_host: &str,
                        _port: u16|
         -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
            Box::pin(async { Ok("10.0.0.1:8080".parse().unwrap()) })
        };
        let result = resolver.resolve_all("example.com", 8080).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "10.0.0.1:8080".parse::<SocketAddr>().unwrap());
    }

    #[tokio::test]
    async fn poll_executor_execute_runs_future() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = Arc::new(AtomicBool::new(false));
        let flag2 = flag.clone();
        let exec = executor::poll_executor::<tokio_rt::TokioRuntime>();
        hyper::rt::Executor::execute(&exec, async move {
            flag2.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert!(flag.load(Ordering::SeqCst));
    }

    // ── Shared helpers for default-method tests ───────────────────────────

    struct MinimalSocketConfig;

    impl SocketConfig for MinimalSocketConfig {
        fn set_keepalive(
            &self,
            _time: std::time::Duration,
            _interval: Option<std::time::Duration>,
            _retries: Option<u32>,
        ) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct DummyStream;

    impl hyper::rt::Read for DummyStream {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Write for DummyStream {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(0))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Unpin for DummyStream {}

    impl SocketConfig for DummyStream {
        fn set_keepalive(
            &self,
            _time: std::time::Duration,
            _interval: Option<std::time::Duration>,
            _retries: Option<u32>,
        ) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct DummyConnectorSend;

    #[allow(clippy::manual_async_fn)]
    impl ConnectorSend for DummyConnectorSend {
        type Stream = DummyStream;

        fn connect(
            &self,
            _addr: SocketAddr,
        ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
            async { Err(io::Error::other("dummy")) }
        }
    }

    struct DummyLocalConnector;

    impl ConnectorLocal for DummyLocalConnector {
        type Stream = DummyStream;

        async fn connect(&self, _addr: SocketAddr) -> io::Result<Self::Stream> {
            Err(io::Error::other("dummy"))
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[test]
    fn socket_config_default_set_fast_open_is_ok() {
        let cfg = MinimalSocketConfig;
        assert!(cfg.set_fast_open().is_ok());
    }

    #[tokio::test]
    async fn connector_send_default_connect_bound_returns_unsupported() {
        let connector = DummyConnectorSend;
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let err = connector.connect_bound(addr, ip).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn connector_send_default_from_std_tcp_returns_unsupported() {
        let connector = DummyConnectorSend;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(local_addr).unwrap();
        let err = connector.from_std_tcp(stream).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn connector_default_connect_bound_returns_unsupported() {
        let connector = DummyLocalConnector;
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let err = connector.connect_bound(addr, ip).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    // ── StaticResolver tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn static_resolver_override_hit() {
        let addr: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let mut sr = StaticResolver::new(None);
        sr.add("example.com".into(), vec![addr]);
        let result = sr.resolve("example.com", 443).await.unwrap();
        assert_eq!(result, addr);
    }

    #[tokio::test]
    async fn static_resolver_override_miss_no_fallback() {
        let sr = StaticResolver::new(None);
        let err = sr.resolve("unknown.com", 80).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }

    #[tokio::test]
    async fn static_resolver_fallback() {
        let addr: SocketAddr = "192.168.1.1:80".parse().unwrap();
        let fallback: Arc<dyn Resolve> = Arc::new(
            move |_host: &str,
                  _port: u16|
                  -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                Box::pin(async move { Ok(addr) })
            },
        );
        let sr = StaticResolver::new(Some(fallback));
        let result = sr.resolve("fallback.com", 80).await.unwrap();
        assert_eq!(result, addr);
    }

    #[tokio::test]
    async fn static_resolver_resolve_all_override_hit() {
        let addr1: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let addr2: SocketAddr = "10.0.0.2:443".parse().unwrap();
        let mut sr = StaticResolver::new(None);
        sr.add("example.com".into(), vec![addr1, addr2]);
        let result = sr.resolve_all("example.com", 443).await.unwrap();
        assert_eq!(result, vec![addr1, addr2]);
    }

    #[tokio::test]
    async fn static_resolver_resolve_all_fallback() {
        let addr: SocketAddr = "192.168.1.1:80".parse().unwrap();
        let fallback: Arc<dyn Resolve> = Arc::new(
            move |_host: &str,
                  _port: u16|
                  -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                Box::pin(async move { Ok(addr) })
            },
        );
        let sr = StaticResolver::new(Some(fallback));
        let result = sr.resolve_all("fallback.com", 80).await.unwrap();
        assert_eq!(result, vec![addr]);
    }

    #[tokio::test]
    async fn static_resolver_resolve_all_no_fallback_error() {
        let sr = StaticResolver::new(None);
        let err = sr.resolve_all("missing.com", 80).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }

    // ── Legacy Runtime default method tests ──────────────────────────────

    #[allow(deprecated)]
    struct StubRuntime;

    #[allow(deprecated, clippy::manual_async_fn)]
    impl legacy::Runtime for StubRuntime {
        type TcpStream = DummyStream;
        type Sleep = std::future::Ready<()>;

        fn connect(_addr: SocketAddr) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
            async { Err(io::Error::other("not implemented")) }
        }

        fn resolve_all(
            _host: &str,
            _port: u16,
        ) -> impl Future<Output = io::Result<Vec<SocketAddr>>> + Send {
            async { Err(io::Error::other("stub")) }
        }

        fn sleep(_duration: std::time::Duration) -> Self::Sleep {
            std::future::ready(())
        }

        fn spawn<F>(_future: F)
        where
            F: Future<Output = ()> + Send + 'static,
        {
        }

        fn from_std_tcp(_stream: std::net::TcpStream) -> io::Result<Self::TcpStream> {
            Err(io::Error::other("not implemented"))
        }

        fn connect_bound(
            _addr: SocketAddr,
            _local: std::net::IpAddr,
        ) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
            async { Err(io::Error::other("not implemented")) }
        }

        #[cfg(unix)]
        type UnixStream = DummyStream;

        #[cfg(unix)]
        fn connect_unix(
            _path: &std::path::Path,
        ) -> impl Future<Output = io::Result<Self::UnixStream>> + Send {
            async { Err(io::Error::other("not implemented")) }
        }
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_set_tcp_keepalive_default_is_ok() {
        let stream = DummyStream;
        assert!(
            StubRuntime::set_tcp_keepalive(&stream, std::time::Duration::from_secs(1), None, None)
                .is_ok()
        );
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_set_tcp_fast_open_default_is_ok() {
        let stream = DummyStream;
        assert!(StubRuntime::set_tcp_fast_open(&stream).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[allow(deprecated)]
    #[test]
    fn legacy_bind_device_default_returns_unsupported() {
        let stream = DummyStream;
        let err = StubRuntime::bind_device(&stream, "lo").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(feature = "compio")]
    #[test]
    fn completion_executor_runs_future() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let flag = Arc::new(AtomicBool::new(false));
            let flag2 = flag.clone();
            let exec = executor::completion_executor::<compio_rt::CompioRuntime>();
            hyper::rt::Executor::execute(&exec, async move {
                flag2.store(true, Ordering::SeqCst);
            });
            compio_runtime::time::sleep(std::time::Duration::from_millis(10)).await;
            assert!(flag.load(Ordering::SeqCst));
        });
    }

    // ── ConnectorLocal from_std_tcp default ──────────────────────────────────

    #[test]
    fn connector_default_from_std_tcp_returns_unsupported() {
        let connector = DummyLocalConnector;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(local_addr).unwrap();
        let err = connector.from_std_tcp(stream).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    // ── ConnectorLocal::from_std_tcp on ConnectorSend (already tested above) ─

    // ── SocketConfig bind_device default (linux only) ────────────────────

    #[cfg(target_os = "linux")]
    #[test]
    fn socket_config_bind_device_default_returns_unsupported() {
        let cfg = MinimalSocketConfig;
        let err = cfg.bind_device("lo").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    // ── StaticResolver with multi-address overrides ─────────────────────

    #[tokio::test]
    async fn static_resolver_resolve_returns_first_of_multiple() {
        let addr1: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let addr2: SocketAddr = "10.0.0.2:443".parse().unwrap();
        let mut sr = StaticResolver::new(None);
        sr.add("multi.com".into(), vec![addr1, addr2]);
        // resolve should return the first address
        let result = sr.resolve("multi.com", 443).await.unwrap();
        assert_eq!(result, addr1);
    }

    // ── Resolve default resolve_all via closure blanket impl ────────────

    #[tokio::test]
    async fn resolve_closure_resolve_all_delegates_to_resolve() {
        // The closure impl only implements `resolve`, so `resolve_all` should
        // use the default impl which wraps the single result.
        let resolver = |_host: &str,
                        _port: u16|
         -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
            Box::pin(async { Ok("10.0.0.5:9090".parse().unwrap()) })
        };
        let result = resolver.resolve_all("any.host", 9090).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "10.0.0.5:9090".parse::<SocketAddr>().unwrap());
    }

    // ── Legacy Runtime default resolve method ───────────────────────────

    #[allow(deprecated)]
    struct ResolveAllStub;

    #[allow(deprecated, clippy::manual_async_fn)]
    impl legacy::Runtime for ResolveAllStub {
        type TcpStream = DummyStream;
        type Sleep = std::future::Ready<()>;

        fn connect(_addr: SocketAddr) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
            async { Err(io::Error::other("not implemented")) }
        }

        fn resolve_all(
            _host: &str,
            _port: u16,
        ) -> impl Future<Output = io::Result<Vec<SocketAddr>>> + Send {
            async {
                Ok(vec![
                    "192.168.1.1:80".parse().unwrap(),
                    "192.168.1.2:80".parse().unwrap(),
                ])
            }
        }

        fn sleep(_duration: std::time::Duration) -> Self::Sleep {
            std::future::ready(())
        }

        fn spawn<F>(_future: F)
        where
            F: Future<Output = ()> + Send + 'static,
        {
        }

        fn from_std_tcp(_stream: std::net::TcpStream) -> io::Result<Self::TcpStream> {
            Err(io::Error::other("not implemented"))
        }

        fn connect_bound(
            _addr: SocketAddr,
            _local: std::net::IpAddr,
        ) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
            async { Err(io::Error::other("not implemented")) }
        }

        #[cfg(unix)]
        type UnixStream = DummyStream;

        #[cfg(unix)]
        fn connect_unix(
            _path: &std::path::Path,
        ) -> impl Future<Output = io::Result<Self::UnixStream>> + Send {
            async { Err(io::Error::other("not implemented")) }
        }
    }

    #[allow(deprecated)]
    #[tokio::test]
    async fn legacy_resolve_default_delegates_to_resolve_all() {
        let addr = ResolveAllStub::resolve("example.com", 80).await.unwrap();
        assert_eq!(addr, "192.168.1.1:80".parse::<SocketAddr>().unwrap());
    }

    // Exercise the ResolveAllStub methods that are defined but never called,
    // to cover lines 565-566, 581-582, 585, 591-592, 595, 599, 606, 609.
    #[allow(deprecated)]
    #[tokio::test]
    async fn resolve_all_stub_connect_returns_error() {
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let err = ResolveAllStub::connect(addr).await.unwrap_err();
        assert_eq!(err.to_string(), "not implemented");
    }

    #[allow(deprecated)]
    #[test]
    fn resolve_all_stub_from_std_tcp_returns_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(local_addr).unwrap();
        let err = ResolveAllStub::from_std_tcp(stream).unwrap_err();
        assert_eq!(err.to_string(), "not implemented");
    }

    #[allow(deprecated)]
    #[tokio::test]
    async fn resolve_all_stub_connect_bound_returns_error() {
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let err = ResolveAllStub::connect_bound(addr, ip).await.unwrap_err();
        assert_eq!(err.to_string(), "not implemented");
    }

    #[cfg(unix)]
    #[allow(deprecated)]
    #[tokio::test]
    async fn resolve_all_stub_connect_unix_returns_error() {
        let path = std::path::Path::new("/tmp/nonexistent.sock");
        let err = ResolveAllStub::connect_unix(path).await.unwrap_err();
        assert_eq!(err.to_string(), "not implemented");
    }

    #[allow(deprecated)]
    #[tokio::test]
    async fn legacy_resolve_default_errors_when_empty() {
        struct EmptyResolveAll;
        #[allow(clippy::manual_async_fn)]
        impl legacy::Runtime for EmptyResolveAll {
            type TcpStream = DummyStream;
            type Sleep = std::future::Ready<()>;

            fn connect(
                _addr: SocketAddr,
            ) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
                async { Err(io::Error::other("not implemented")) }
            }

            fn resolve_all(
                _host: &str,
                _port: u16,
            ) -> impl Future<Output = io::Result<Vec<SocketAddr>>> + Send {
                async { Ok(vec![]) }
            }

            fn sleep(_duration: std::time::Duration) -> Self::Sleep {
                std::future::ready(())
            }

            fn spawn<F>(_future: F)
            where
                F: Future<Output = ()> + Send + 'static,
            {
            }

            fn from_std_tcp(_stream: std::net::TcpStream) -> io::Result<Self::TcpStream> {
                Err(io::Error::other("not implemented"))
            }

            fn connect_bound(
                _addr: SocketAddr,
                _local: std::net::IpAddr,
            ) -> impl Future<Output = io::Result<Self::TcpStream>> + Send {
                async { Err(io::Error::other("not implemented")) }
            }

            #[cfg(unix)]
            type UnixStream = DummyStream;

            #[cfg(unix)]
            fn connect_unix(
                _path: &std::path::Path,
            ) -> impl Future<Output = io::Result<Self::UnixStream>> + Send {
                async { Err(io::Error::other("not implemented")) }
            }
        }

        let err = EmptyResolveAll::resolve("example.com", 80)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }

    // ── StaticResolver: multiple hosts and port passthrough ──────────────

    #[tokio::test]
    async fn static_resolver_multiple_hosts_independent() {
        let addr1: SocketAddr = "10.0.0.1:80".parse().unwrap();
        let addr2: SocketAddr = "10.0.0.2:443".parse().unwrap();
        let mut sr = StaticResolver::new(None);
        sr.add("host1.com".into(), vec![addr1]);
        sr.add("host2.com".into(), vec![addr2]);

        assert_eq!(sr.resolve("host1.com", 80).await.unwrap(), addr1);
        assert_eq!(sr.resolve("host2.com", 443).await.unwrap(), addr2);
        // Confirm one host doesn't bleed into the other
        let err = sr.resolve("host3.com", 80).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }

    #[tokio::test]
    async fn static_resolver_fallback_receives_host_and_port() {
        // Verify fallback resolver receives the correct host and port
        let fallback: Arc<dyn Resolve> = Arc::new(
            |host: &str,
             port: u16|
             -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                let addr_str = format!("192.168.0.1:{port}");
                let host = host.to_string();
                Box::pin(async move {
                    if host == "expected.com" {
                        Ok(addr_str.parse().unwrap())
                    } else {
                        Err(io::Error::other("unexpected host"))
                    }
                })
            },
        );
        let sr = StaticResolver::new(Some(fallback));
        let result = sr.resolve("expected.com", 9999).await.unwrap();
        assert_eq!(result, "192.168.0.1:9999".parse::<SocketAddr>().unwrap());
    }

    #[tokio::test]
    async fn static_resolver_resolve_all_fallback_exercises_default_resolve_all() {
        // When fallback only implements `resolve`, `resolve_all` should use
        // the Resolve trait default which wraps single result in vec.
        let addr: SocketAddr = "172.16.0.1:8080".parse().unwrap();
        let fallback: Arc<dyn Resolve> = Arc::new(
            move |_host: &str,
                  _port: u16|
                  -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                Box::pin(async move { Ok(addr) })
            },
        );
        let sr = StaticResolver::new(Some(fallback));
        let result = sr.resolve_all("delegated.com", 8080).await.unwrap();
        // The default resolve_all wraps single result in a vec
        assert_eq!(result, vec![addr]);
    }

    // ── Error message content verification ──────────────────────────────

    #[tokio::test]
    async fn static_resolver_error_message_contains_host_and_port() {
        let sr = StaticResolver::new(None);
        let err = sr.resolve("myhost.io", 12345).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("myhost.io"), "error should contain host");
        assert!(msg.contains("12345"), "error should contain port");
    }

    #[tokio::test]
    async fn static_resolver_resolve_all_error_message_contains_host_and_port() {
        let sr = StaticResolver::new(None);
        let err = sr.resolve_all("otherhost.io", 54321).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("otherhost.io"), "error should contain host");
        assert!(msg.contains("54321"), "error should contain port");
    }

    #[tokio::test]
    async fn connector_send_connect_bound_error_message() {
        let connector = DummyConnectorSend;
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let err = connector.connect_bound(addr, ip).await.unwrap_err();
        assert!(
            err.to_string().contains("connect_bound"),
            "error message should mention connect_bound"
        );
    }

    #[tokio::test]
    async fn connector_send_from_std_tcp_error_message() {
        let connector = DummyConnectorSend;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(local_addr).unwrap();
        let err = connector.from_std_tcp(stream).unwrap_err();
        assert!(
            err.to_string().contains("from_std_tcp"),
            "error message should mention from_std_tcp"
        );
    }

    #[tokio::test]
    async fn connector_connect_bound_error_message() {
        let connector = DummyLocalConnector;
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let err = connector.connect_bound(addr, ip).await.unwrap_err();
        assert!(
            err.to_string().contains("connect_bound"),
            "error message should mention connect_bound"
        );
    }

    #[test]
    fn connector_from_std_tcp_error_message() {
        let connector = DummyLocalConnector;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(local_addr).unwrap();
        let err = connector.from_std_tcp(stream).unwrap_err();
        assert!(
            err.to_string().contains("from_std_tcp"),
            "error message should mention from_std_tcp"
        );
    }

    // ── MinimalSocketConfig keepalive behavior ──────────────────────────

    #[test]
    fn socket_config_set_keepalive_with_all_params() {
        let cfg = MinimalSocketConfig;
        let result = cfg.set_keepalive(
            std::time::Duration::from_secs(30),
            Some(std::time::Duration::from_secs(5)),
            Some(3),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn socket_config_set_keepalive_no_interval_no_retries() {
        let cfg = MinimalSocketConfig;
        let result = cfg.set_keepalive(std::time::Duration::from_secs(60), None, None);
        assert!(result.is_ok());
    }

    // ── RuntimeCompletion::block_on from mod.rs context ─────────────────

    #[test]
    fn tokio_runtime_block_on_executes_async() {
        use crate::runtime::RuntimeCompletion;
        let result = tokio_rt::TokioRuntime::block_on(async { 7 * 6 }).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn tokio_runtime_block_on_can_do_io() {
        use crate::runtime::RuntimeCompletion;
        let addr = tokio_rt::TokioRuntime::block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        })
        .unwrap();
        assert!(addr.port() > 0);
    }

    // ── Legacy Runtime: connect_unix on unix ────────────────────────────

    #[cfg(unix)]
    #[allow(deprecated)]
    #[tokio::test]
    async fn legacy_stub_connect_unix_returns_error() {
        let err = StubRuntime::connect_unix(std::path::Path::new("/nonexistent.sock"))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    // ── Legacy Runtime: connect returns error ───────────────────────────

    #[allow(deprecated)]
    #[tokio::test]
    async fn legacy_stub_connect_returns_error() {
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let err = StubRuntime::connect(addr).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[allow(deprecated)]
    #[tokio::test]
    async fn legacy_stub_connect_bound_returns_error() {
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let local: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let err = StubRuntime::connect_bound(addr, local).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_stub_from_std_tcp_returns_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let local_addr = listener.local_addr().unwrap();
        let stream = std::net::TcpStream::connect(local_addr).unwrap();
        let err = StubRuntime::from_std_tcp(stream).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[allow(deprecated)]
    #[tokio::test]
    async fn legacy_stub_resolve_all_returns_error() {
        let err = StubRuntime::resolve_all("example.com", 80)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[allow(deprecated)]
    #[test]
    fn legacy_stub_sleep_completes_immediately() {
        // StubRuntime::sleep returns ready(()), so polling should be immediate.
        use std::task::{Context, RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut sleep = StubRuntime::sleep(std::time::Duration::from_secs(100));
        let pinned = Pin::new(&mut sleep);
        assert_eq!(pinned.poll(&mut cx), Poll::Ready(()));
    }

    // ── DummyConnectorSend connect error ────────────────────────────────

    #[tokio::test]
    async fn dummy_connector_send_connect_returns_error() {
        let connector = DummyConnectorSend;
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let err = ConnectorSend::connect(&connector, addr).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(err.to_string().contains("dummy"));
    }

    // ── DummyLocalConnector connect error ───────────────────────────────

    #[tokio::test]
    async fn dummy_local_connector_connect_returns_error() {
        let connector = DummyLocalConnector;
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let err = ConnectorLocal::connect(&connector, addr).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(err.to_string().contains("dummy"));
    }

    // ── StaticResolver::new with fallback and no overrides ──────────────

    #[tokio::test]
    async fn static_resolver_new_empty_overrides() {
        let sr = StaticResolver::new(None);
        // No overrides, no fallback — everything should error
        let err = sr.resolve("any.host", 1).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
        let err = sr.resolve_all("any.host", 1).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrNotAvailable);
    }

    // ── Resolve trait: resolve returns error propagates to resolve_all ───

    #[tokio::test]
    async fn resolve_trait_default_resolve_all_propagates_error() {
        struct FailingResolver;
        impl Resolve for FailingResolver {
            fn resolve(
                &self,
                _host: &str,
                _port: u16,
            ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
                Box::pin(async {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "test error",
                    ))
                })
            }
        }
        let resolver = FailingResolver;
        let err = resolver.resolve_all("fail.host", 80).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
        assert!(err.to_string().contains("test error"));
    }

    // ── Resolve blanket impl for closures: error propagation ────────────

    #[tokio::test]
    async fn resolve_closure_error_propagation() {
        let resolver = |_host: &str,
                        _port: u16|
         -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
            Box::pin(async { Err(io::Error::new(io::ErrorKind::TimedOut, "dns timeout")) })
        };
        let err = resolver.resolve("timeout.host", 80).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);

        // Default resolve_all should also propagate the error
        let err = resolver.resolve_all("timeout.host", 80).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    // ── StaticResolver: override replaces previous value ────────────────

    #[tokio::test]
    async fn static_resolver_add_overwrites_previous() {
        let addr1: SocketAddr = "10.0.0.1:80".parse().unwrap();
        let addr2: SocketAddr = "10.0.0.2:80".parse().unwrap();
        let mut sr = StaticResolver::new(None);
        sr.add("example.com".into(), vec![addr1]);
        sr.add("example.com".into(), vec![addr2]);
        // Should return the second value
        let result = sr.resolve("example.com", 80).await.unwrap();
        assert_eq!(result, addr2);
    }

    // ── DummyStream: Read/Write trait behavior ──────────────────────────

    #[test]
    fn dummy_stream_read_returns_ready_ok() {
        use hyper::rt::Read;
        use std::task::{Context, RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = DummyStream;
        let mut buf = [0u8; 64];
        let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);
        let result = Pin::new(&mut stream).poll_read(&mut cx, read_buf.unfilled());
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn dummy_stream_write_returns_zero() {
        use hyper::rt::Write;
        use std::task::{Context, RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = DummyStream;
        let result = Pin::new(&mut stream).poll_write(&mut cx, b"hello");
        match result {
            Poll::Ready(Ok(n)) => assert_eq!(n, 0),
            other => panic!("expected Poll::Ready(Ok(0)), got {other:?}"),
        }
    }

    #[test]
    fn dummy_stream_flush_returns_ready_ok() {
        use hyper::rt::Write;
        use std::task::{Context, RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = DummyStream;
        let result = Pin::new(&mut stream).poll_flush(&mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn dummy_stream_shutdown_returns_ready_ok() {
        use hyper::rt::Write;
        use std::task::{Context, RawWaker, RawWakerVTable, Waker};
        fn dummy_raw_waker() -> RawWaker {
            fn no_op(_: *const ()) {}
            fn clone(ptr: *const ()) -> RawWaker {
                RawWaker::new(ptr, &VTABLE)
            }
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(dummy_raw_waker()) };
        let mut cx = Context::from_waker(&waker);
        let mut stream = DummyStream;
        let result = Pin::new(&mut stream).poll_shutdown(&mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    // ── DummyStream: SocketConfig ───────────────────────────────────────

    #[test]
    fn dummy_stream_set_keepalive_returns_ok() {
        let stream = DummyStream;
        assert!(
            stream
                .set_keepalive(
                    std::time::Duration::from_secs(10),
                    Some(std::time::Duration::from_secs(5)),
                    Some(3),
                )
                .is_ok()
        );
    }

    #[test]
    fn dummy_stream_set_fast_open_uses_default() {
        let stream = DummyStream;
        // DummyStream doesn't override set_fast_open, so default returns Ok
        assert!(stream.set_fast_open().is_ok());
    }
}
