use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

/// Base async scheduling — timer support for all runtimes.
///
/// This is the foundation for all runtimes. Every runtime (tokio, smol, compio,
/// wasm) implements this trait. It provides only the `sleep` primitive; spawning
/// is split into [`RuntimeLocal`] (completion-based) and [`RuntimePoll`]
/// (work-stealing) to prevent runtime-inappropriate spawn calls from compiling.
pub trait RuntimeCompletion: 'static {
    /// A sleep future returned by the runtime.
    type Sleep: Future<Output = ()>;

    /// Sleep for the given duration.
    fn sleep(duration: Duration) -> Self::Sleep;

    /// Run a future to completion on a new runtime instance.
    ///
    /// Creates a single-threaded runtime and blocks the current thread until
    /// the future completes. Used by [`BlockingClient`](crate::blocking::BlockingClient).
    fn block_on<F: Future>(future: F) -> Result<F::Output, crate::error::Error>;
}

/// Thread-local spawning for completion-based runtimes (compio, wasm).
///
/// The spawned future does **not** need to be `Send` — it will never migrate
/// between threads. This is the natural model for io_uring/IOCP and
/// single-threaded environments.
///
/// Poll-based runtimes (tokio, smol) do **not** implement this trait;
/// use [`RuntimePoll::spawn_send`] instead.
pub trait RuntimeLocal: RuntimeCompletion {
    /// Spawn a `!Send` future on the current thread's event loop.
    fn spawn_local<F: Future<Output = ()> + 'static>(future: F);
}

/// Poll-based runtime superset — work-stealing, `Send` futures.
///
/// Runtimes that support migrating tasks between worker threads (tokio, smol)
/// implement this trait in addition to [`RuntimeCompletion`].
pub trait RuntimePoll: RuntimeCompletion<Sleep: Send> + Send + Sync {
    /// Spawn a `Send` future that can be stolen by any worker thread.
    fn spawn_send<F: Future<Output = ()> + Send + 'static>(future: F);
}

/// Post-connection socket configuration (keepalive, fast open, device binding).
///
/// Implemented per-runtime on stream adapter types (`TokioIo`, `SmolIo`, etc.).
/// Used by [`HttpEngineSend`](crate::HttpEngineSend) to apply socket options after
/// connection establishment.
pub trait SocketConfig {
    /// Configure TCP keepalive on this stream.
    fn set_keepalive(
        &self,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()>;

    /// Enable TCP Fast Open (RFC 7413) on this stream.
    fn set_fast_open(&self) -> io::Result<()> {
        Ok(())
    }

    /// Bind this stream to a network interface (Linux only).
    #[cfg(target_os = "linux")]
    fn bind_device(&self, _interface: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interface binding not supported by this connector",
        ))
    }
}

/// Produces raw byte streams from resolved socket addresses.
///
/// No `Send`/`Sync` bounds — compio connectors are legitimately `!Send`
/// because `compio_net::TcpStream` is tied to the thread-local event loop.
/// For poll-based runtimes (tokio, smol), implement [`ConnectorSend`] instead
/// which guarantees `Send` futures.
// TODO: rename to `ConnectorLocal` for symmetry with `ConnectorSend`.
#[allow(async_fn_in_trait)]
pub trait Connector: 'static {
    /// The byte stream type passed to hyper for HTTP framing.
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin + SocketConfig + 'static;

    /// Establish a connection to a pre-resolved socket address.
    async fn connect(&self, addr: SocketAddr) -> io::Result<Self::Stream>;

    /// Connect while binding to a specific local IP address.
    async fn connect_bound(
        &self,
        _addr: SocketAddr,
        _local: std::net::IpAddr,
    ) -> io::Result<Self::Stream> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "connect_bound not supported by this connector",
        ))
    }

    /// Convert a `std::net::TcpStream` into the connector's stream type.
    #[allow(clippy::wrong_self_convention)]
    fn from_std_tcp(&self, stream: std::net::TcpStream) -> io::Result<Self::Stream> {
        let _ = stream;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "from_std_tcp not supported by this connector",
        ))
    }

    /// Convert the connector's stream type into a blocking `std::net::TcpStream`.
    #[allow(clippy::wrong_self_convention)]
    fn into_std_tcp(&self, stream: Self::Stream) -> io::Result<std::net::TcpStream> {
        let _ = stream;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "into_std_tcp not supported by this connector",
        ))
    }
}

/// `Send`-safe connector for poll-based runtimes (tokio, smol).
///
/// Same interface as [`Connector`] but with `Send` futures. This is the trait
/// used by [`HttpEngineSend`](crate::HttpEngineSend) and the tower connector layer.
/// Completion-based runtimes (compio) implement only [`Connector`].
pub trait ConnectorSend: Clone + Send + Sync + 'static {
    /// The byte stream type passed to hyper for HTTP framing.
    type Stream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + SocketConfig + 'static;

    /// Establish a connection to a pre-resolved socket address.
    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send;

    /// Connect while binding to a specific local IP address.
    fn connect_bound(
        &self,
        _addr: SocketAddr,
        _local: std::net::IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        async {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "connect_bound not supported by this connector",
            ))
        }
    }

    /// Convert a `std::net::TcpStream` into the connector's stream type.
    #[allow(clippy::wrong_self_convention)]
    fn from_std_tcp(&self, stream: std::net::TcpStream) -> io::Result<Self::Stream> {
        let _ = stream;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "from_std_tcp not supported by this connector",
        ))
    }

    /// Convert the connector's stream type into a blocking `std::net::TcpStream`.
    #[allow(clippy::wrong_self_convention)]
    fn into_std_tcp(&self, stream: Self::Stream) -> io::Result<std::net::TcpStream> {
        let _ = stream;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "into_std_tcp not supported by this connector",
        ))
    }
}
