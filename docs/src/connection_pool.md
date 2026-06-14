# Connection Pool

aioduct maintains a connection pool to reuse TCP (and TLS) connections across requests to the same origin, avoiding the overhead of repeated handshakes.

## How It Works

### Pool Key

Connections are keyed by `(scheme, authority, protocol hint, proxy route)` — for example, `(https, api.example.com:443, Auto, direct)`. Two requests share a pooled connection only when all four fields match, except for the explicit h2/h3 coalescing path described below. The proxy route component keeps direct traffic separate from proxied traffic, and also separates different proxy configurations for the same origin.

### Lifecycle

1. **Checkout**: When a request is made, the pool checks for an existing idle connection to the target pool key. It uses LIFO ordering (most recently returned first) to prefer the freshest connections. Each candidate is checked for readiness and maximum lifetime — if a connection is stale, closed, too old, or saturated by the active stream cap, it's skipped or discarded and the next one is tried.
2. **Reserve**: If no reusable connection is available and `pool_max_active_per_host` is configured, a fresh connection attempt atomically reserves an active slot before DNS or TCP dialing. This prevents connection bursts from opening more concurrent fresh sockets than the configured cap. When the cap is reached, the request fails immediately with a typed [`PoolLimitKind::MaxActivePerHost`][pool-limit] error.
3. **Send**: The request is sent on the connection (either reused or freshly established).
4. **Checkin**: HTTP/2 and HTTP/3 connections return to the pool immediately because they can multiplex concurrent streams. HTTP/1.1 connections return only after the response body has drained and the sender is ready again. Connections past `pool_max_lifetime` are not checked back in. When the idle queue is at capacity, the oldest idle connection is evicted to make room for the new one.

### Idle Eviction

Connections are evicted in three ways:
- **On checkout**: Expired connections (past idle timeout) are discarded while searching for a ready one.
- **On checkin**: When the per-host queue is full, the oldest connection is evicted.
- **Background reaper**: A periodic background task runs at the idle timeout interval and removes all expired connections, preventing memory leaks from unused hosts.

### Limits

`pool_max_idle_per_host(n)` controls how many idle handles are retained per pool key after requests complete. It does not limit the number of in-flight requests by itself.

`pool_max_active_per_host(n)` controls currently checked-out handles plus fresh connection attempts for the same pool key. Use it to cap concurrent sockets/handles toward one origin or proxy route. When the cap is reached, new requests fail immediately with a typed [`PoolLimitKind::MaxActivePerHost`][pool-limit] error. A value of `0` disables the active cap and leaves it unlimited.

`pool_max_active_streams_per_connection(n)` is different: it applies only to HTTP/2 and HTTP/3 multiplexed connections and caps how many concurrent stream handles can be cloned from one pooled transport. HTTP/1.1 has no multiplexed streams, so this limit does not affect HTTP/1.1.

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
    .pool_max_idle_per_host(10)                  // default: 10
    .pool_max_active_per_host(64)                // default: unlimited
    .pool_max_active_streams_per_connection(100) // default: unlimited
    .build()?;
```

The builder methods compose fluently and are applied to the underlying
`ConnectionPool` before the client is built.

### Options

| Option                                    | Default   | Description                                          |
|-------------------------------------------|-----------|------------------------------------------------------|
| `pool_idle_timeout`                       | 90s       | How long an idle connection is kept before eviction   |
| `pool_max_lifetime`                       | none      | Maximum connection age before it stops being reused   |
| `pool_max_idle_per_host`                  | 10        | Maximum idle connections per (scheme, authority)      |
| `pool_max_active_per_host`                | unlimited | Maximum checked-out handles and fresh connection attempts per pool key; 0 disables the cap |
| `pool_max_active_streams_per_connection`  | unlimited | Maximum active HTTP/2 or HTTP/3 streams per connection |

[pool-limit]: https://docs.rs/aioduct/latest/aioduct/enum.PoolLimitKind.html

Tokio, smol, compio, and blocking clients use this native pool. Wasm and
wasi-p2 transports are platform-managed, so pooling and DNS reuse behavior are
provided by the browser or WASI host rather than by `ConnectionPool`.

## Connection Health

On checkout, the pool verifies each candidate connection is still ready using hyper's `SendRequest::is_ready()`. If a connection has been closed by the server (e.g., due to keep-alive timeout), it's discarded and the next pooled connection is tried. If no ready connection is found, a new one is established.

## Diagnostics

`pool_stats()` returns a `PoolStats` snapshot of the pool's lifetime counters and current inventory. It is available on both `HttpEngineSend` and `HttpEngineLocal` (and therefore on the runtime client aliases). Counters are monotonic since engine creation and live in atomics outside the pool mutex, so reading them is cheap and never blocks request hot paths.

```rust,no_run
use aioduct::TokioClient;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();

client.get("https://example.com/")?.send().await?;
client.get("https://example.com/")?.send().await?;

let stats = client.pool_stats();
println!("hits={} misses={}", stats.checkout_hits, stats.checkout_misses);
println!("idle={} active={}", stats.idle_pool_entries, stats.checked_out_pool_handles);

for host in &stats.hosts {
    println!(
        "{}://{} ({}, route={}): {} idle, {} active",
        host.scheme, host.authority, host.protocol_hint, host.route, host.idle, host.active,
    );
}
# Ok(())
# }
```

### `PoolStats` fields

| Field                          | Type             | Meaning                                                                 |
|--------------------------------|------------------|-------------------------------------------------------------------------|
| `checkout_hits`                | `u64`            | Checkouts that found an idle connection in the pool                      |
| `checkout_coalesced_hits`      | `u64`            | Checkouts reused via SAN-based coalescing (always 0 on local engines)   |
| `checkout_misses`              | `u64`            | Requests that exhausted all pool paths and required a fresh connection   |
| `stale_reuse_retries`          | `u64`            | Connections detected as stale mid-request and transparently retried      |
| `idle_timeout_evictions`       | `u64`            | Connections evicted due to idle timeout expiry                           |
| `max_lifetime_evictions`       | `u64`            | Connections evicted due to exceeding their maximum lifetime              |
| `checkout_not_ready_evictions` | `u64`            | Connections discarded at checkout because `is_ready()` returned false    |
| `capacity_evictions`           | `u64`            | Connections evicted because the per-host idle queue was at capacity      |
| `idle_pool_entries`            | `usize`          | Idle pool handles across all hosts (current)                            |
| `checked_out_pool_handles`     | `usize`          | Checked-out pool handles across all hosts (current)                     |
| `hosts`                        | `Vec<PoolHostStats>` | Per-host breakdown, sorted by `(scheme, authority)`                  |

Each `PoolHostStats` carries `scheme`, `authority`, `protocol_hint` (`Auto`/`H2c`/`AdaptiveH2c`), `route` (`"direct"` or an opaque proxy-route label), and the host's current `idle` / `active` handle counts.

Counts reflect pool-internal handle tracking, which can differ from physical connection counts for H2/H3 multiplexed transports — one transport may back several checked-out handles. `checkout_coalesced_hits` is always 0 on local engines because connection coalescing is a send-path feature.

The CLI surfaces these stats directly: `aioduct http -v` shows a pool summary, and `aioduct download` reports pool counters and inventory alongside its progress output.

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
