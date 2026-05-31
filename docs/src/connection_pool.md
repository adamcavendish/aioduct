# Connection Pool

aioduct maintains a connection pool to reuse TCP (and TLS) connections across requests to the same origin, avoiding the overhead of repeated handshakes.

## How It Works

### Pool Key

Connections are keyed by `(scheme, authority)` — for example, `(https, api.example.com:443)`. Two requests to the same origin share pooled connections; requests to different origins use separate pools.

### Lifecycle

1. **Checkout**: When a request is made, the pool checks for an existing idle connection to the target origin. It uses LIFO ordering (most recently returned first) to prefer the freshest connections. Each candidate is checked for readiness — if a connection is stale or closed, it's discarded and the next one is tried.
2. **Send**: The request is sent on the connection (either reused or freshly established).
3. **Checkin**: After the response headers are received, the connection is returned to the pool. When at capacity, the oldest idle connection is evicted to make room for the new one.

### Idle Eviction

Connections are evicted in three ways:
- **On checkout**: Expired connections (past idle timeout) are discarded while searching for a ready one.
- **On checkin**: When the per-host queue is full, the oldest connection is evicted.
- **Background reaper**: A periodic background task runs at the idle timeout interval and removes all expired connections, preventing memory leaks from unused hosts.

### HTTP/2 Multiplexing

HTTP/2 connections support multiplexing — multiple concurrent requests share a single connection. The pool tracks the hyper `SendRequest` handle, which naturally supports this. When an h2 connection is checked out, it remains usable by other requests concurrently.

By default, aioduct does not cap active multiplexed streams per connection. Use `pool_max_active_streams_per_connection(n)` to limit how many active HTTP/2 or HTTP/3 stream handles may be cloned from one pooled connection at a time. The value must be greater than 0. HTTP/1.1 connections are not affected.

### HTTP/3 (QUIC) Pooling

When the `http3` feature is enabled with the rustls backend and one rustls provider, QUIC connections are pooled alongside TCP connections. Like HTTP/2, HTTP/3 multiplexes streams over a single connection, so a pooled QUIC connection can serve multiple sequential requests to the same origin without re-establishing the handshake. The pool uses the same `(scheme, authority)` key for both TCP and QUIC connections.

## Configuration

```rust,no_run
use std::time::Duration;
use aioduct::TokioClient;

let client = TokioClient::builder()
    .pool_idle_timeout(Duration::from_secs(90))  // default: 90s
    .pool_max_lifetime(Duration::from_secs(600)) // default: none
    .pool_max_idle_per_host(10)                   // default: 10
    .pool_max_active_streams_per_connection(100)  // default: unlimited
    .build()?;
```

### Options

| Option                                    | Default   | Description                                          |
|-------------------------------------------|-----------|------------------------------------------------------|
| `pool_idle_timeout`                       | 90s       | How long an idle connection is kept before eviction   |
| `pool_max_lifetime`                       | none      | Maximum connection age before it stops being reused   |
| `pool_max_idle_per_host`                  | 10        | Maximum idle connections per (scheme, authority)      |
| `pool_max_active_streams_per_connection`  | unlimited | Maximum active HTTP/2 or HTTP/3 streams per connection |

## Connection Health

On checkout, the pool verifies each candidate connection is still ready using hyper's `SendRequest::is_ready()`. If a connection has been closed by the server (e.g., due to keep-alive timeout), it's discarded and the next pooled connection is tried. If no ready connection is found, a new one is established.

## Connection Coalescing

When enabled (default), aioduct reuses h2/h3 connections for different hostnames that share the same TLS certificate, matching browser behavior per [RFC 7540 §9.1.1](https://www.rfc-editor.org/rfc/rfc7540#section-9.1.1).

### How It Works

1. When a new request has no pooled connection for its origin, the pool scans existing h2/h3 connections.
2. If a connection's TLS certificate includes the target hostname in its Subject Alternative Names (SANs), **and** the resolved IP address matches the connection's remote address, the connection is reused.
3. This avoids a redundant TLS handshake and TCP/QUIC connection for hosts that share infrastructure (e.g., `api.example.com` and `cdn.example.com` on the same certificate).

### Configuration

```rust,no_run
use aioduct::TokioClient;

// Enabled by default; disable if needed:
let client = TokioClient::builder()
    .connection_coalescing(false)
    .build()?;
```

### Requirements

- Only applies to **h2 and h3** connections (HTTP/1.1 doesn't multiplex).
- Requires the `rustls` feature (SANs are extracted from the peer certificate).
- Both SAN match and IP match are required — this prevents coalescing across servers that happen to share a wildcard certificate but serve different content.
