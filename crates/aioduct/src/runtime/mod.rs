use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

/// Abstraction over async runtimes (tokio, smol, compio).
#[allow(async_fn_in_trait)]
pub trait Runtime: Send + Sync + 'static {
    /// The runtime's TCP stream type.
    type TcpStream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;
    /// A sleep future returned by the runtime.
    type Sleep: Future<Output = ()> + Send + Sync;

    /// Connect to a remote address over TCP.
    fn connect(addr: SocketAddr) -> impl Future<Output = io::Result<Self::TcpStream>> + Send;
    /// Resolve a hostname to a socket address.
    ///
    /// The default implementation delegates to [`Runtime::resolve_all`] and
    /// returns the first address.
    async fn resolve(host: &str, port: u16) -> io::Result<SocketAddr> {
        let addrs = Self::resolve_all(host, port).await?;
        addrs
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses resolved"))
    }
    /// Resolve a hostname to all available socket addresses.
    fn resolve_all(
        host: &str,
        port: u16,
    ) -> impl Future<Output = io::Result<Vec<SocketAddr>>> + Send;
    /// Sleep for the given duration.
    fn sleep(duration: Duration) -> Self::Sleep;
    /// Spawn a background task.
    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static;

    /// Configure TCP keepalive on a stream.
    fn set_tcp_keepalive(
        _stream: &Self::TcpStream,
        _time: Duration,
        _interval: Option<Duration>,
        _retries: Option<u32>,
    ) -> io::Result<()> {
        Ok(())
    }

    /// Bind a TCP stream to a network interface (Linux only).
    #[cfg(target_os = "linux")]
    fn bind_device(_stream: &Self::TcpStream, _interface: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interface binding not supported by this runtime",
        ))
    }

    /// Convert a `std::net::TcpStream` into the runtime's stream type.
    fn from_std_tcp(stream: std::net::TcpStream) -> io::Result<Self::TcpStream>;

    /// Connect to a remote address, binding to a specific local IP.
    fn connect_bound(
        addr: SocketAddr,
        local: std::net::IpAddr,
    ) -> impl Future<Output = io::Result<Self::TcpStream>> + Send;

    /// The runtime's Unix domain socket stream type.
    #[cfg(unix)]
    type UnixStream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;

    /// Connect to a Unix domain socket.
    #[cfg(unix)]
    fn connect_unix(path: &Path) -> impl Future<Output = io::Result<Self::UnixStream>> + Send;
}

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

/// Executor adapter that delegates to `R::spawn` for hyper's HTTP/2 handshake.
pub(crate) struct HyperExecutor<R>(PhantomData<fn() -> R>);

impl<R> Clone for HyperExecutor<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for HyperExecutor<R> {}

impl<R, F> hyper::rt::Executor<F> for HyperExecutor<R>
where
    R: Runtime,
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        R::spawn(fut);
    }
}

/// Create a [`HyperExecutor`] for the given runtime.
pub(crate) fn hyper_executor<R: Runtime>() -> HyperExecutor<R> {
    HyperExecutor(PhantomData)
}

/// Tokio runtime implementation.
#[cfg(feature = "tokio")]
pub mod tokio_rt;
#[cfg(feature = "tokio")]
pub use tokio_rt::TokioRuntime;

/// Smol runtime implementation.
#[cfg(feature = "smol")]
pub mod smol_rt;
#[cfg(feature = "smol")]
pub use smol_rt::SmolRuntime;

/// Compio runtime implementation.
#[cfg(feature = "compio")]
pub mod compio_rt;
#[cfg(feature = "compio")]
pub use compio_rt::CompioRuntime;
