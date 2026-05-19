# Architecture

## Module Layout

```
src/
  lib.rs              # Re-exports, compile_error gate, type aliases
  error.rs            # Error enum, type aliases
  engine.rs           # HttpEngineCore<B>, HttpEngineSend<R,C>, HttpEngineLocal<R,C>
  engine_builder.rs   # HttpEngineBuilder<R,C> — fluent client configuration
  request.rs          # RequestBuilderSend<R,C>, RequestBuilderLocal<R,C>
  response.rs         # ResponseBodySend, ResponseBodyLocal — status, headers, body consumption
  body.rs             # BodyStream, RequestBody (buffered/streaming)
  timeout.rs          # Pin-projected Timeout future
  cookie.rs           # CookieJar, Cookie, Set-Cookie parsing
  cache.rs            # HttpCache, CacheConfig — in-memory HTTP cache
  retry.rs            # RetryConfig, RetryBudget — exponential backoff
  throttle.rs         # RateLimiter — token-bucket rate limiting
  bandwidth.rs        # BandwidthLimiter — byte-rate download throttle
  redirect.rs         # RedirectPolicy, RedirectAction
  middleware.rs       # Middleware trait
  digest_auth.rs      # DigestAuth — HTTP Digest challenge-response
  netrc.rs            # Netrc, NetrcMiddleware — .netrc credential injection
  happy_eyeballs.rs   # RFC 6555 IPv6/IPv4 connection racing
  sse.rs              # SseStream, SseEvent — Server-Sent Events
  multipart.rs        # Multipart, Part — multipart/form-data
  chunk_download.rs   # ChunkDownload — parallel range requests
  upgrade.rs          # Upgraded — HTTP/1.1 protocol upgrade
  decompress.rs       # DecompressBody — gzip/brotli/zstd/deflate
  proxy.rs            # ProxyConfig, ProxySettings, NoProxy
  socks4.rs           # SOCKS4/4a handshake
  socks5.rs           # SOCKS5 handshake
  blocking.rs         # Blocking client wrapper (requires tokio)
  traits.rs           # HttpClient, RequestBuilderExt, ResponseExt, ByteStreamExt
  runtime/
    mod.rs            # RuntimeCompletion, RuntimePoll, RuntimeLocal traits
    tokio_rt.rs       # TokioRuntime, TcpConnector, TokioIo
    smol_rt.rs        # SmolRuntime, TcpConnector, SmolIo
    compio_rt.rs      # CompioRuntime, TcpConnector
  connector.rs        # ConnectorSend, ConnectorLocal traits, SocketConfig
  pool/
    mod.rs            # ConnectionPool — keyed pooling
    connection.rs     # PooledConnection, HttpConnection enum
  tls/
    mod.rs            # TlsConnect trait, re-exports
    rustls_connector.rs  # RustlsConnector, TlsStream, ALPN
  h3/
    mod.rs            # HTTP/3 transport (experimental)
  http2.rs            # Http2Config
  hickory.rs          # HickoryResolver (requires hickory-dns)
  wasm/               # WASM/browser runtime
```

## Request Flow

A request in aioduct goes through these stages:

```
client.get("http://example.com/path")?
  -> RequestBuilderSend (accumulate headers, body, timeout, query params)
  -> RequestBuilderSend::send()
    -> apply timeout wrapper (Timeout future)
    -> rate limiter wait (if configured)
    -> check HTTP cache (if configured, return cached response on hit)
    -> HttpEngineCore::execute()
      -> merge default headers
      -> apply cookie jar cookies (if configured)
      -> retry loop (if configured):
        -> redirect loop (up to max_redirects):
          -> run middleware on_request hooks
          -> build http::Request with method, path-only URI, headers
          -> execute_single()
            -> pool checkout (reuse existing connection?)
            -> if miss: ConnectorSend::connect(&SocketConfig)
              -> TCP connect -> TLS handshake (if HTTPS)
            -> ALPN -> select h1 or h2 sender
            -> send request on connection
            -> pool checkin
          -> digest auth retry (if 401 + WWW-Authenticate: Digest)
          -> run middleware on_response hooks
          -> store response cookies in jar (if configured)
          -> check redirect status -> follow or return
      -> cache response (if configured and cacheable)
      -> decompress body (if content-encoding matches)
      -> apply bandwidth limiter (if configured)
  -> ResponseBodySend
```

## Key Design Decisions

### No hyper-util

hyper 1.x provides raw connection-level primitives. hyper-util wraps them in a legacy `Client` that mimics hyper 0.x behavior. aioduct skips hyper-util entirely and implements:

- **IO adapters** (TokioIo, SmolIo): Bridge runtime-specific `AsyncRead`/`AsyncWrite` to `hyper::rt::Read`/`hyper::rt::Write`. Each is ~50 lines of unsafe pin projection.
- **HyperExecutor**: A generic executor that delegates `spawn` to the active Runtime. Uses `PhantomData<fn() -> R>` (not `PhantomData<R>`) to ensure it is always `Unpin`, which hyper's h2 handshake requires.

### Split Engine Types: Send vs Local

The v0.2 architecture splits the client into two engine types to cleanly support both poll-based and completion-based runtimes:

- **`HttpEngineSend<R: RuntimePoll, C: ConnectorSend>`** — for runtimes where futures are `Send` (tokio, smol). The connector produces streams that are `Send`, enabling work-stealing schedulers.
- **`HttpEngineLocal<R: RuntimeLocal, C: ConnectorLocal>`** — for thread-per-core runtimes (compio) where futures are `!Send`. The connector produces streams that stay on the local thread.

Both share `HttpEngineCore<B>` for configuration state (pool settings, timeouts, middleware, TLS, etc.), minimizing code duplication.

### Connector Abstraction

Networking is decoupled from the runtime via connector traits:

- **`ConnectorSend`**: `Clone + Send + Sync + 'static`, connects asynchronously, returns a `Send` stream.
- **`ConnectorLocal`**: `'static`, connects asynchronously, returns a `!Send` stream.

Each runtime module provides a default `TcpConnector` that implements the appropriate trait. Users can supply custom connectors for testing, proxying, or alternative transports.

### Generic over Runtime

`HttpEngineSend<R, C>` and `HttpEngineLocal<R, C>` carry the runtime and connector as type parameters rather than using dynamic dispatch. This means:

- Zero-cost abstraction — no vtable overhead
- All runtime-specific code is monomorphized away
- The compiler can inline across the runtime boundary

### Portable Traits

The `HttpClient`, `RequestBuilderExt`, `ResponseExt`, and `ByteStreamExt` traits provide a common interface that works across both `Send` and `Local` engine variants, enabling generic code that is runtime-agnostic.

### Connection Pool

The pool is keyed by `(scheme, authority)` and stores connections in a `VecDeque` per key. On checkout, expired connections are evicted. On checkin, the pool respects `max_idle_per_host`. HTTP/2 connections can be shared across concurrent requests since h2 multiplexes streams.

### TLS State Machine

The rustls integration implements an async TLS handshake as a manual state machine. Because `rustls::ClientConnection` expects synchronous `std::io::Read`/`Write`, the adapter uses helper functions that wrap async streams and return `WouldBlock` when the underlying stream would block. This avoids spawning a blocking task or using a separate thread for the handshake.

### Timeout via Pin Projection

The `Timeout` type is a pin-projected enum with two variants:

- `NoTimeout { future }` — passes through directly
- `WithTimeout { future, sleep }` — polls both; if sleep completes first, returns `Error::Timeout`

This avoids `tokio::select!` or any runtime-specific timeout mechanism, keeping the implementation runtime-agnostic.
