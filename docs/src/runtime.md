# Runtime and Connector Traits

aioduct is runtime-agnostic. The runtime and connector traits define the minimal interfaces that an async runtime and its networking layer must provide.

## Runtime Trait Hierarchy

The runtime system is split into a three-level trait hierarchy, from most general to most capable:

```rust
pub trait RuntimeCompletion: 'static {
    fn block_on<F: Future>(future: F) -> F::Output;
}

pub trait RuntimePoll: RuntimeCompletion {
    fn spawn_send<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static;

    fn sleep(duration: Duration) -> impl Future<Output = ()> + Send;
}

pub trait RuntimeLocal: RuntimeCompletion {
    fn spawn_local<F>(future: F)
    where
        F: Future<Output = ()> + 'static;

    fn sleep(duration: Duration) -> impl Future<Output = ()>;
}
```

### RuntimeCompletion (Base)

The foundation trait. Provides `block_on` to drive a future to completion synchronously. Every runtime implements this.

### RuntimePoll (Send-capable runtimes)

Extends `RuntimeCompletion` with:

- **`spawn_send`**: Spawn a `Send` future as a detached background task. Used for driving hyper connection futures on work-stealing schedulers.
- **`sleep`**: Create a `Send` sleep future for the given duration. Used for timeouts and pool idle eviction.

Implemented by `TokioRuntime` and `SmolRuntime`.

### RuntimeLocal (Thread-local runtimes)

Extends `RuntimeCompletion` with:

- **`spawn_local`**: Spawn a `!Send` future on the current thread. Used for thread-per-core runtimes where tasks never cross thread boundaries.
- **`sleep`**: Create a sleep future (not required to be `Send`).

Implemented by `CompioRuntime`.

## Connector Traits

Networking is decoupled from the runtime via connector traits. Each connector is responsible for establishing a TCP connection given a `SocketConfig`.

```rust
pub trait ConnectorSend: Clone + Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    fn connect(&self, config: &SocketConfig) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

pub trait ConnectorLocal: 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + 'static;

    fn connect(&self, config: &SocketConfig) -> impl Future<Output = io::Result<Self::Stream>>;
}
```

### ConnectorSend

For use with `HttpEngineSend<R, C>`. Must be `Clone + Send + Sync` so it can be shared across tasks on a work-stealing scheduler. The returned stream must be `Send`.

### ConnectorLocal

For use with `HttpEngineLocal<R, C>`. No `Send` bounds — the connector and its streams live on a single thread.

### SocketConfig

Both connector traits receive a `SocketConfig` that contains the target address, port, DNS resolution hints, TCP options (nodelay, keepalive), and local bind address.

## Built-in Implementations

### TokioRuntime + TcpConnector

Enabled with `features = ["tokio"]`.

```rust
use aioduct::TokioClient;

let client = TokioClient::new();
```

- `TokioRuntime` implements `RuntimePoll` using `tokio::runtime::Handle::block_on`, `tokio::spawn`, and `tokio::time::sleep`.
- `tokio_rt::TcpConnector` implements `ConnectorSend` using `tokio::net::TcpStream`. Sets `TCP_NODELAY` by default.
- The `TokioIo` adapter bridges tokio's `AsyncRead`/`AsyncWrite` to hyper's `rt::Read`/`rt::Write`.

### SmolRuntime + TcpConnector

Enabled with `features = ["smol"]`.

```rust
use aioduct::SmolClient;

let client = SmolClient::new();
```

- `SmolRuntime` implements `RuntimePoll` using `smol::block_on`, `smol::spawn`, and `async_io::Timer`.
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

- `CompioRuntime` implements `RuntimeLocal` using `compio_runtime::block_on`, `compio_runtime::spawn`, and compio's native timers.
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
