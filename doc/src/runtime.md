# Runtime Trait

aioduct is runtime-agnostic. The `Runtime` trait defines the minimal interface that an async runtime must provide.

## Trait Definition

```rust
pub trait Runtime: Send + Sync + 'static {
    type TcpStream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;
    type Sleep: Future<Output = ()> + Send;

    async fn connect(addr: SocketAddr) -> io::Result<Self::TcpStream>;
    async fn resolve(host: &str, port: u16) -> io::Result<SocketAddr>;
    fn sleep(duration: Duration) -> Self::Sleep;
    fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static;
}
```

### Associated Types

- **`TcpStream`**: The runtime's TCP connection type, wrapped in an IO adapter that implements `hyper::rt::Read + hyper::rt::Write`. For tokio this is `TokioIo<tokio::net::TcpStream>`, for smol it's `SmolIo<smol::net::TcpStream>`.
- **`Sleep`**: A future that resolves after a duration. Used for timeouts and pool idle eviction.

### Required Methods

- **`connect`**: Establish a TCP connection to the given address. Implementations should set `TCP_NODELAY`.
- **`resolve`**: DNS resolution. Converts a hostname and port to a `SocketAddr`.
- **`sleep`**: Create a sleep future for the given duration.
- **`spawn`**: Spawn a detached background task. Used for driving hyper connection futures.

## Built-in Implementations

### TokioRuntime

Enabled with `features = ["tokio"]`.

```rust
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::new();
```

Uses `tokio::net::TcpStream`, `tokio::time::sleep`, and `tokio::spawn`. The `TokioIo` adapter bridges tokio's `AsyncRead`/`AsyncWrite` to hyper's `rt::Read`/`rt::Write`.

### SmolRuntime

Enabled with `features = ["smol"]`.

```rust
use aioduct::Client;
use aioduct::runtime::SmolRuntime;

let client = Client::<SmolRuntime>::new();
```

Uses `smol::net::TcpStream`, `async_io::Timer`, and `smol::spawn`. The `SmolIo` adapter bridges `futures_io::AsyncRead`/`AsyncWrite` to hyper's traits.

### CompioRuntime (Experimental)

Enabled with `features = ["compio"]`.

```rust
use aioduct::Client;
use aioduct::runtime::CompioRuntime;

compio_runtime::Runtime::new().unwrap().block_on(async {
    let client = Client::<CompioRuntime>::new();
    let resp = client.get("http://httpbin.org/get").unwrap().send().await.unwrap();
    println!("status: {}", resp.status());
});
```

Compio is a completion-based I/O runtime (io_uring on Linux, IOCP on Windows) with a thread-per-core execution model. Since hyper requires readiness-based polling (`poll_read`/`poll_write`), the CompioRuntime uses `async-io` for TCP I/O as a compatibility bridge, while using compio's native runtime for task spawning, timers, and DNS resolution.

The `CompioIo` adapter bridges `futures_io::AsyncRead`/`AsyncWrite` (from `async-io::Async<TcpStream>`) to `hyper::rt::Read`/`hyper::rt::Write`, following the same pattern as the SmolRuntime.

**Important**: compio futures are `!Send` (they cannot be sent between threads). The CompioRuntime uses `unsafe impl Send` wrappers since compio's thread-per-core model guarantees futures never actually cross thread boundaries. This is safe as long as the `Client<CompioRuntime>` is only used within a single compio runtime thread.

## HyperExecutor

hyper's HTTP/2 handshake requires an `Executor` to spawn background tasks for connection management. aioduct provides a generic `HyperExecutor<R>` that delegates to `R::spawn`:

```rust
pub struct HyperExecutor<R>(PhantomData<fn() -> R>);
```

The `PhantomData<fn() -> R>` (rather than `PhantomData<R>`) ensures `HyperExecutor` is always `Unpin` regardless of `R`, which hyper's h2 handshake requires.

## Implementing a Custom Runtime

To add a new runtime, implement the `Runtime` trait and provide an IO adapter. The IO adapter must implement `hyper::rt::Read` and `hyper::rt::Write` by delegating to the runtime's native async IO traits. See `src/runtime/tokio_rt.rs` for a reference implementation.
