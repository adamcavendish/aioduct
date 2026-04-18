# Architecture

## Module Layout

```
src/
  lib.rs              # Re-exports, compile_error gate
  error.rs            # Error enum, type aliases
  client.rs           # Client<R> and ClientBuilder<R>
  request.rs          # RequestBuilder<R> — fluent request API
  response.rs         # Response — status, headers, body consumption
  timeout.rs          # Pin-projected Timeout future
  runtime/
    mod.rs            # Runtime trait, HyperExecutor<R>
    tokio_rt.rs       # TokioRuntime, TokioIo, TokioSleep
    smol_rt.rs        # SmolRuntime, SmolIo, SmolSleep
    compio_rt.rs      # CompioRuntime (placeholder)
  pool/
    mod.rs            # ConnectionPool<R> — keyed pooling
    connection.rs     # PooledConnection, HttpConnection enum
  tls/
    mod.rs            # TlsConnect trait, re-exports
    rustls_connector.rs  # RustlsConnector, TlsStream, ALPN
  h3/
    mod.rs            # HTTP/3 transport (placeholder)
```

## Request Flow

A request in aioduct goes through these stages:

```
Client::get("http://example.com/path")
  → RequestBuilder (accumulate headers, body, timeout, query params)
  → RequestBuilder::send()
    → apply timeout wrapper (Timeout future)
    → Client::execute()
      → merge default headers
      → redirect loop (up to max_redirects):
        → build http::Request with method, path-only URI, headers
        → Client::execute_single()
          → pool checkout (reuse existing connection?)
          → if miss: DNS resolve → TCP connect → TLS handshake (if HTTPS)
          → ALPN → select h1 or h2 sender
          → send request on connection
          → pool checkin
        → check redirect status → follow or return
  → Response
```

## Key Design Decisions

### No hyper-util

hyper 1.x provides raw connection-level primitives. hyper-util wraps them in a legacy `Client` that mimics hyper 0.x behavior. aioduct skips hyper-util entirely and implements:

- **IO adapters** (TokioIo, SmolIo): Bridge runtime-specific `AsyncRead`/`AsyncWrite` to `hyper::rt::Read`/`hyper::rt::Write`. Each is ~50 lines of unsafe pin projection.
- **HyperExecutor**: A generic executor that delegates `spawn` to the active Runtime. Uses `PhantomData<fn() -> R>` (not `PhantomData<R>`) to ensure it is always `Unpin`, which hyper's h2 handshake requires.

### Generic over Runtime

`Client<R: Runtime>` carries the runtime as a type parameter rather than using dynamic dispatch. This means:

- Zero-cost abstraction — no vtable overhead
- All runtime-specific code is monomorphized away
- The compiler can inline across the runtime boundary

### Connection Pool

The pool is keyed by `(scheme, authority)` and stores connections in a `VecDeque` per key. On checkout, expired connections are evicted. On checkin, the pool respects `max_idle_per_host`. HTTP/2 connections can be shared across concurrent requests since h2 multiplexes streams.

### TLS State Machine

The rustls integration implements an async TLS handshake as a manual state machine. Because `rustls::ClientConnection` expects synchronous `std::io::Read`/`Write`, the adapter uses helper functions that wrap async streams and return `WouldBlock` when the underlying stream would block. This avoids spawning a blocking task or using a separate thread for the handshake.

### Timeout via Pin Projection

The `Timeout` type is a pin-projected enum with two variants:

- `NoTimeout { future }` — passes through directly
- `WithTimeout { future, sleep }` — polls both; if sleep completes first, returns `Error::Timeout`

This avoids `tokio::select!` or any runtime-specific timeout mechanism, keeping the implementation runtime-agnostic.
