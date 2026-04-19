# Architecture

## Module Layout

```
src/
  lib.rs              # Re-exports, compile_error gate
  error.rs            # Error enum, type aliases
  client.rs           # Client<R> and ClientBuilder<R>
  request.rs          # RequestBuilder<R> — fluent request API
  response.rs         # Response — status, headers, body consumption
  body.rs             # BodyStream, RequestBody (buffered/streaming)
  timeout.rs          # Pin-projected Timeout future
  cookie.rs           # CookieJar, Cookie, Set-Cookie parsing
  cache.rs            # HttpCache, CacheConfig — in-memory HTTP cache
  retry.rs            # RetryConfig, RetryBudget — exponential backoff
  throttle.rs         # RateLimiter — token-bucket rate limiting
  redirect.rs         # RedirectPolicy, RedirectAction
  middleware.rs       # Middleware trait
  sse.rs              # SseStream, SseEvent — Server-Sent Events
  multipart.rs        # Multipart, Part — multipart/form-data
  chunk_download.rs   # ChunkDownload — parallel range requests
  upgrade.rs          # Upgraded — HTTP/1.1 protocol upgrade
  decompress.rs       # DecompressBody — gzip/brotli/zstd/deflate
  proxy.rs            # ProxyConfig, ProxySettings, NoProxy
  socks4.rs           # SOCKS4/4a handshake
  socks5.rs           # SOCKS5 handshake
  blocking.rs         # Blocking client wrapper (requires tokio)
  runtime/
    mod.rs            # Runtime trait, HyperExecutor<R>
    tokio_rt.rs       # TokioRuntime, TokioIo, TokioSleep
    smol_rt.rs        # SmolRuntime, SmolIo, SmolSleep
    compio_rt.rs      # CompioRuntime
  pool/
    mod.rs            # ConnectionPool<R> — keyed pooling
    connection.rs     # PooledConnection, HttpConnection enum
  tls/
    mod.rs            # TlsConnect trait, re-exports
    rustls_connector.rs  # RustlsConnector, TlsStream, ALPN
  h3/
    mod.rs            # HTTP/3 transport (experimental)
  http2.rs            # Http2Config
  connector.rs        # Tower Service connector (requires tower)
  hickory.rs          # HickoryResolver (requires hickory-dns)
  wasm/               # WASM/browser runtime
```

## Request Flow

A request in aioduct goes through these stages:

```
Client::get("http://example.com/path")
  → RequestBuilder (accumulate headers, body, timeout, query params)
  → RequestBuilder::send()
    → apply timeout wrapper (Timeout future)
    → rate limiter wait (if configured)
    → check HTTP cache (if configured, return cached response on hit)
    → Client::execute()
      → merge default headers
      → apply cookie jar cookies (if configured)
      → retry loop (if configured):
        → redirect loop (up to max_redirects):
          → run middleware on_request hooks
          → build http::Request with method, path-only URI, headers
          → Client::execute_single()
            → pool checkout (reuse existing connection?)
            → if miss: DNS resolve → TCP connect → TLS handshake (if HTTPS)
            → ALPN → select h1 or h2 sender
            → send request on connection
            → pool checkin
          → run middleware on_response hooks
          → store response cookies in jar (if configured)
          → check redirect status → follow or return
      → cache response (if configured and cacheable)
      → decompress body (if content-encoding matches)
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
