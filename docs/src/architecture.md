# Architecture

## Module Layout

```text
src/
  lib.rs                  # Public exports, feature gates, client aliases
  client/                 # Engines, builders, request flow, dispatch, replay,
                          # connection deadlines, and proxy establishment
  request/                # RequestBuilderSend and RequestBuilderLocal
  response/               # Response and body transforms/consumption
  body/                   # Buffered and streaming request/response bodies
  forward/                # Forward builders, dispatch plans, targets, headers,
                          # and trailer policy
  proxy/                  # Proxy configuration, immutable routes, chains,
                          # bypass rules, and establishment plans
  connector/              # ConnectorSend and ConnectorLocal
  runtime/                # Runtime traits, executors, resolvers, and adapters
  pool/                   # Pool keys, connection handles, and accounting
  tls/                    # TLS traits plus the rustls connector state machine
  h3/                     # HTTP/3 request lifecycle and Quinn adapter
  message_signatures/     # RFC 9421 parsing, signing, and verification
  upgrade/                # UpgradedSend and UpgradedLocal
  chunk_download/         # Send/local parallel range downloaders
  cache/ cookie/ sse/     # Higher-level HTTP facilities
  wasm.rs / wasi_p2.rs    # Platform-managed guest transports
  wasmtime/               # Host-side WASI HTTP adapter
```

The tree above shows ownership boundaries rather than every supporting module.
Protocol helpers such as HTTP/2 configuration, framing validation, SOCKS
handshakes, redirects, digest authentication, and observability remain
top-level modules where they are shared by several dispatch paths.

## Request Flow

A request in aioduct goes through these stages:

```text
client.get("http://example.com/path")?
  -> RequestBuilderSend (method, URI, headers, body, protocol, timeouts)
  -> RequestBuilderSend::send()
    -> apply HSTS, default headers, cookies, middleware, and cache validators
    -> classify body replayability and finalize digest/signature metadata
    -> capture the finalized request state used by eligible retries
    -> for each redirect, digest retry, or configured retry attempt:
      -> resolve one immutable ProxyDispatchRoute
      -> select the exact/negotiable protocol and full pool key
      -> check out a matching H1, H2, or H3 transport
      -> on a pool miss, run one connection-acquisition deadline across
         coordination, DNS, TCP/QUIC, proxy negotiation, TLS, and handshake
      -> send the request with evidence-gated stale-connection recovery
      -> supervise upload completion and return response headers
    -> apply response middleware, redirects, cookies, cache policy,
       decompression, read timeout, and bandwidth limiting
  -> ResponseBodySend
```

The Local engine follows the same policy with `RequestBuilderLocal`, local
futures, and local connector/transport implementations. Forwarding bypasses
ordinary client middleware but enters the same dispatch layer after its
upstream target, protocol, and hop-field policy have been finalized. See
[Request Dispatch Guarantees](request_dispatch.md) for replay, proxy, timeout,
and protocol boundaries.

## Key Design Decisions

### No hyper-util

hyper 1.x provides raw connection-level primitives. hyper-util wraps them in a legacy `Client` that mimics hyper 0.x behavior. aioduct skips hyper-util entirely and implements:

- **IO adapters** (TokioIo, SmolIo): Bridge runtime-specific `AsyncRead`/`AsyncWrite` to `hyper::rt::Read`/`hyper::rt::Write`. Each is ~50 lines of unsafe pin projection.
- **HTTP/2 task executors**: Separate internal `PollExecutor` and
  `CompletionExecutor` implementations delegate to the active runtime's
  `spawn_send` or `spawn_local` operation. Both use `PhantomData<fn() -> R>` so
  the executor type does not inherit unnecessary ownership or auto-trait bounds
  from the runtime marker.

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

The pool key contains `(scheme, authority, protocol hint, proxy route, forced
transport endpoint, effective HTTP/3 endpoint)`. The complete proxy route keeps
direct connections separate from each distinct proxy configuration. Forced
addresses cannot satisfy ordinary checkouts or requests forced to another
address, and the HTTP/3 endpoint prevents an Alt-Svc change from reusing a QUIC
connection to an older endpoint.

Connections are stored in a `VecDeque` per full key. On checkout, expired
connections are evicted. On checkin, the pool respects `max_idle_per_host`.
HTTP/2 and HTTP/3 connections can be shared across concurrent requests because
they multiplex streams.

### TLS State Machine

The rustls integration implements an async TLS handshake as a manual state machine. Because `rustls::ClientConnection` expects synchronous `std::io::Read`/`Write`, the adapter uses helper functions that wrap async streams and return `WouldBlock` when the underlying stream would block. This avoids spawning a blocking task or using a separate thread for the handshake.

### Timeout via Pin Projection

The `Timeout` type is a pin-projected enum with two variants:

- `NoTimeout { future }` — passes through directly
- `WithTimeout { future, sleep }` — polls both; if sleep completes first, returns `Error::Timeout`

This avoids `tokio::select!` or any runtime-specific timeout mechanism, keeping the implementation runtime-agnostic.
