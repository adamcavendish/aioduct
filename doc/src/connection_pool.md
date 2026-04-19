# Connection Pool

aioduct maintains a connection pool to reuse TCP (and TLS) connections across requests to the same origin, avoiding the overhead of repeated handshakes.

## How It Works

### Pool Key

Connections are keyed by `(scheme, authority)` — for example, `(https, api.example.com:443)`. Two requests to the same origin share pooled connections; requests to different origins use separate pools.

### Lifecycle

1. **Checkout**: When a request is made, the pool checks for an existing idle connection to the target origin. Expired connections (past idle timeout) are evicted during checkout.
2. **Send**: The request is sent on the connection (either reused or freshly established).
3. **Checkin**: After the response headers are received, the connection is returned to the pool for future reuse — provided it's still alive and the pool hasn't reached `max_idle_per_host`.

### HTTP/2 Multiplexing

HTTP/2 connections support multiplexing — multiple concurrent requests share a single connection. The pool tracks the hyper `SendRequest` handle, which naturally supports this. When an h2 connection is checked out, it remains usable by other requests concurrently.

### HTTP/3 (QUIC) Pooling

When the `http3` feature is enabled, QUIC connections are pooled alongside TCP connections. Like HTTP/2, HTTP/3 multiplexes streams over a single connection, so a pooled QUIC connection can serve multiple sequential requests to the same origin without re-establishing the handshake. The pool uses the same `(scheme, authority)` key for both TCP and QUIC connections.

## Configuration

```rust,no_run
use std::time::Duration;
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .pool_idle_timeout(Duration::from_secs(90))  // default: 90s
    .pool_max_idle_per_host(10)                   // default: 10
    .build();
```

### Options

| Option                  | Default | Description                                        |
|-------------------------|---------|----------------------------------------------------|
| `pool_idle_timeout`     | 90s     | How long an idle connection is kept before eviction |
| `pool_max_idle_per_host`| 10      | Maximum idle connections per (scheme, authority)    |

## Connection Health

On checkout, the pool verifies the connection is still ready using hyper's `SendRequest::is_ready()`. If the connection has been closed by the server (e.g., due to keep-alive timeout), it's discarded and a new connection is established.
