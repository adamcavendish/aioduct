# Runtime and Connector Traits

aioduct is runtime-agnostic. The runtime and connector traits define the minimal interfaces that an async runtime and its networking layer must provide.

## Runtime Trait Hierarchy

The runtime system uses one shared completion trait with separate Send-capable
and thread-local execution traits:

```rust
pub trait RuntimeCompletion: 'static {
    type Sleep: Future<Output = ()>;

    fn sleep(duration: Duration) -> Self::Sleep;
    fn block_on<F: Future>(future: F) -> Result<F::Output, aioduct::Error>;
}

pub trait RuntimePoll: RuntimeCompletion<Sleep: Send> + Send + Sync {
    fn spawn_send<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static;
}

pub trait RuntimeLocal: RuntimeCompletion {
    fn spawn_local<F>(future: F)
    where
        F: Future<Output = ()> + 'static;
}
```

### RuntimeCompletion (Base)

The foundation trait. It defines the runtime's sleep future and provides
`block_on` to drive a future to completion on a new runtime instance. Runtime
construction can fail, so `block_on` returns `Result`. Every native runtime
implements this trait.

### RuntimePoll (Send-capable runtimes)

Extends `RuntimeCompletion` with:

- **`spawn_send`**: Spawn a `Send` future as a detached background task. Used for driving hyper connection futures on work-stealing schedulers.

Its `RuntimeCompletion::Sleep` future must also be `Send`.

Implemented by `TokioRuntime` and `SmolRuntime`.

### RuntimeLocal (Thread-local runtimes)

Extends `RuntimeCompletion` with:

- **`spawn_local`**: Spawn a `!Send` future on the current thread. Used for thread-per-core runtimes where tasks never cross thread boundaries.

Its `RuntimeCompletion::Sleep` future does not need to be `Send`.

Implemented by `CompioRuntime`.

## Connector Traits

Networking is decoupled from the runtime via connector traits. DNS resolution
happens before connector dispatch, so each connector establishes a stream to a
pre-resolved `SocketAddr`.

```rust
pub trait ConnectorSend: Clone + Send + Sync + 'static {
    type Stream: hyper::rt::Read
        + hyper::rt::Write
        + SocketConfig
        + Unpin
        + Send
        + 'static;

    fn connect(
        &self,
        addr: SocketAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

pub trait ConnectorLocal: 'static {
    type Stream: hyper::rt::Read
        + hyper::rt::Write
        + SocketConfig
        + Unpin
        + 'static;

    async fn connect(&self, addr: SocketAddr) -> io::Result<Self::Stream>;
}
```

### ConnectorSend

For use with `HttpEngineSend<R, C>`. Must be `Clone + Send + Sync` so it can be shared across tasks on a work-stealing scheduler. The returned stream must be `Send`.

### ConnectorLocal

For use with `HttpEngineLocal<R, C>`. No `Send` bounds — the connector and its streams live on a single thread.

Both connector traits also support `connect_bound` for a requested local
address and `from_std_tcp` for adopting a socket created by Happy Eyeballs.

### SocketConfig

Connector stream types implement `SocketConfig`. The engine uses that trait
after connection establishment to apply TCP keepalive, TCP Fast Open, and
interface binding where the platform supports them. Destination addresses and
local bind addresses are passed to connector methods rather than stored in a
configuration object.

## Built-in Implementations

### TokioRuntime + TcpConnector

Enabled with `features = ["tokio"]`.

```rust
use aioduct::TokioClient;

let client = TokioClient::new();
```

- `TokioRuntime` creates a current-thread runtime for `block_on`, rejects
  blocking use from inside an active Tokio runtime, and implements
  `RuntimePoll` with `tokio::spawn` plus `tokio::time::sleep`.
- `tokio_rt::TcpConnector` implements `ConnectorSend` using `tokio::net::TcpStream`. Sets `TCP_NODELAY` by default.
- The `TokioIo` adapter bridges tokio's `AsyncRead`/`AsyncWrite` to hyper's `rt::Read`/`rt::Write`.

### SmolRuntime + TcpConnector

Enabled with `features = ["smol"]`.

```rust
use aioduct::SmolClient;

let client = SmolClient::new();
```

- `SmolRuntime` implements `RuntimeCompletion` and `RuntimePoll` using
  `smol::block_on`, `smol::spawn`, and `async_io::Timer`.
- `smol_rt::TcpConnector` implements `ConnectorSend` using `smol::net::TcpStream`.
- The `SmolIo` adapter bridges `futures_io::AsyncRead`/`AsyncWrite` to hyper's traits.

### CompioRuntime + TcpConnector (Experimental)

Enabled with `features = ["compio"]`.

```rust
use aioduct::CompioClient;

compio_runtime::Runtime::new().unwrap().block_on(async {
    let client = CompioClient::new();
    let resp = client.get("http://httpbin.org/get")?.send().await?;
    println!("status: {}", resp.status());
    Ok::<_, aioduct::Error>(())
});
```

Compio is a completion-based I/O runtime (io_uring on Linux, IOCP on Windows) with a thread-per-core execution model.

- `CompioRuntime` creates a `compio_runtime::Runtime` for `block_on`, uses
  `compio_runtime::spawn` for local tasks, and wraps `async_io::Timer` for the
  shared runtime sleep contract.
- `compio_rt::TcpConnector` implements `ConnectorLocal`. Streams are `!Send` since they are bound to the completion ring of the current thread.

**Important**: compio futures are `!Send` (they cannot be sent between threads). The `CompioClient` type alias uses `HttpEngineLocal`, which does not require `Send` bounds on futures or streams. This is safe because compio's thread-per-core model guarantees futures never cross thread boundaries.

## HTTP/2 Task Executors

hyper's HTTP/2 client handshake requires an `Executor` to drive background
connection tasks. aioduct uses separate internal executors for the two runtime
models: `PollExecutor<R>` delegates to `RuntimePoll::spawn_send`, while
`CompletionExecutor<R>` delegates to `RuntimeLocal::spawn_local`.

Both use `PhantomData<fn() -> R>` so the executor type does not inherit
unnecessary ownership or auto-trait bounds from the runtime marker. These
executors are internal connection-lifecycle machinery, not part of the public
runtime trait contract.

## Implementing a Custom Runtime

To add a new poll-based runtime:

1. Implement `RuntimeCompletion` and `RuntimePoll` for your runtime marker type.
2. Implement `ConnectorSend` for a connector struct that establishes TCP connections using your runtime's networking primitives.
3. Provide an IO adapter that implements `hyper::rt::Read` and `hyper::rt::Write` by delegating to your runtime's native async IO traits.

For a thread-local runtime, implement `RuntimeCompletion` and `RuntimeLocal` instead, with a `ConnectorLocal` implementation.

See `src/runtime/tokio_rt.rs` for a reference `RuntimePoll` + `ConnectorSend` implementation.
