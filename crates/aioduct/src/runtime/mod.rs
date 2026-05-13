pub(crate) mod executor;
mod legacy;
mod traits;

pub use traits::{
    Connector, ConnectorSend, RuntimeCompletion, RuntimeLocal, RuntimePoll, SocketConfig,
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
pub(crate) struct StaticResolver {
    overrides: std::collections::HashMap<String, Vec<SocketAddr>>,
    fallback: Option<Arc<dyn Resolve>>,
}

impl StaticResolver {
    pub(crate) fn new(fallback: Option<Arc<dyn Resolve>>) -> Self {
        Self {
            overrides: std::collections::HashMap::new(),
            fallback,
        }
    }

    pub(crate) fn add(&mut self, host: String, addrs: Vec<SocketAddr>) {
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

    #[test]
    fn poll_executor_clone_and_copy() {
        let exec = executor::poll_executor::<tokio_rt::TokioRuntime>();
        #[allow(clippy::clone_on_copy)]
        let _cloned = exec.clone();
        let _copied = exec;
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

    impl Connector for DummyLocalConnector {
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
}
