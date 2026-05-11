use std::future::Future;
use std::io;
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

/// Abstraction over async runtimes (tokio, smol, compio).
#[deprecated(
    since = "0.2.0",
    note = "Use `RuntimeCompletion` or `RuntimePoll` instead. Will be removed in 0.3.0."
)]
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

    /// Enable TCP Fast Open on a connected stream (RFC 7413).
    ///
    /// On Linux this sets `TCP_FASTOPEN_CONNECT`, which causes the kernel to
    /// use TFO for subsequent connections to the same destination.
    fn set_tcp_fast_open(_stream: &Self::TcpStream) -> io::Result<()> {
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
