# Request Forwarding

aioduct includes a built-in request forwarding builder for reverse proxy and API gateway use cases. It strips hop-by-hop headers, rewrites the URI to target an upstream, streams the body without buffering, and bypasses all client middleware (redirects, cookies, cache, decompression).

## Basic Forwarding

```rust,no_run
use aioduct::TokioClient;
use aioduct::runtime::tokio_rt::TcpConnector;
use bytes::Bytes;
use http_body_util::Full;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new(TcpConnector);

// Incoming request from your framework (axum, actix, hyper, etc.)
let incoming_req = http::Request::builder()
    .method("GET")
    .uri("/api/users?page=2")
    .header("host", "public-gateway.example.com")
    .body(Full::new(Bytes::new()))
    .unwrap();

let resp = client
    .forward(incoming_req)
    .upstream("http://backend:8080".parse::<http::Uri>().unwrap())
    .strip_prefix("/api")       // /api/users → /users
    .send()
    .await?;

println!("status: {}", resp.status());
# Ok(())
# }
```

## Builder Methods

| Method | Description |
|--------|-------------|
| `.upstream(uri)` | Target upstream origin (required) |
| `.strip_prefix(prefix)` | Remove a path prefix before forwarding |
| `.preserve_host()` | Keep the original Host header instead of rewriting to upstream |
| `.timeout(duration)` | Per-request timeout |
| `.header(name, value)` | Inject an extra header |
| `.forward_header(name)` | Copy a named header through hop-by-hop stripping |
| `.remove_header(name)` | Remove a header before sending |
| `.on_request(fn)` | Mutate request parts just before sending |
| `.on_response(fn)` | Mutate the response before returning |
| `.h2c()` | Force HTTP/2 prior knowledge (h2c) on this forward |
| `.adaptive_h2c()` | Probe h2c, fall back to h1; result cached per-authority |
| `.upgrade()` | Force upgrade header preservation (usually auto-detected) |

## Hop-by-Hop Header Stripping

`ForwardBuilder` automatically strips these headers from both the incoming request and the upstream response:

- `Connection`
- `Keep-Alive`
- `Proxy-Authenticate`
- `Proxy-Authorization`
- `Proxy-Connection`
- `TE`
- `Trailer`
- `Transfer-Encoding`

Use `.forward_header(name)` to preserve specific headers through stripping.

## WebSocket / HTTP Upgrade Forwarding

Upgrade requests are auto-detected and handled correctly:

### HTTP/1.1 Upgrade

When `Connection: Upgrade` is present, `ForwardBuilder`:
- Preserves `Connection` and `Upgrade` headers through hop-by-hop stripping
- Forces HTTP/1.1 on the upstream connection
- Skips response hop-by-hop stripping (101 is terminal)

```rust,no_run
use aioduct::TokioClient;
use aioduct::runtime::tokio_rt::TcpConnector;
use bytes::Bytes;
use http_body_util::Full;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new(TcpConnector);

let ws_req = http::Request::builder()
    .method("GET")
    .uri("/ws/chat")
    .header("connection", "Upgrade")
    .header("upgrade", "websocket")
    .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
    .header("sec-websocket-version", "13")
    .body(Full::new(Bytes::new()))
    .unwrap();

let resp = client
    .forward(ws_req)
    .upstream("http://ws-backend:9000".parse::<http::Uri>().unwrap())
    .send()
    .await?;

assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);

// Get the bidirectional tunnel
let mut upstream_io = resp.upgrade().await?;

// In a real proxy, splice with downstream:
// tokio::io::copy_bidirectional(&mut downstream_io, &mut upstream_io).await?;
# Ok(())
# }
```

### HTTP/2 Extended CONNECT (RFC 8441)

When the request method is `CONNECT` and a `Protocol` extension is present, `ForwardBuilder`:
- Forces HTTP/2 on the upstream connection
- Uses the full URI (not path-only) so hyper generates correct pseudo-headers
- Skips response hop-by-hop stripping

```rust,no_run
use aioduct::{TokioClient, Protocol};
use aioduct::runtime::tokio_rt::TcpConnector;
use bytes::Bytes;
use http_body_util::Full;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::builder(TcpConnector)
    .http2_prior_knowledge()
    .build()?;

let mut req = http::Request::builder()
    .method(http::Method::CONNECT)
    .uri("http://h2-backend:8080/ws/chat")
    .body(Full::new(Bytes::new()))
    .unwrap();
req.extensions_mut().insert(Protocol::from_static("websocket"));

let resp = client
    .forward(req)
    .upstream("http://h2-backend:8080".parse::<http::Uri>().unwrap())
    .send()
    .await?;

assert_eq!(resp.status(), http::StatusCode::OK);

let mut upstream_io = resp.upgrade().await?;
// Bidirectional tunnel is ready
# Ok(())
# }
```

## Hooks

Use `on_request` and `on_response` for transformations not covered by other builder methods:

```rust,no_run
# use aioduct::TokioClient;
# use aioduct::runtime::tokio_rt::TcpConnector;
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
# let client = TokioClient::new(TcpConnector);
# let incoming_req = http::Request::builder().uri("/test").body(Full::new(Bytes::new())).unwrap();
let resp = client
    .forward(incoming_req)
    .upstream("http://backend:8080".parse::<http::Uri>().unwrap())
    .on_request(|parts| {
        parts.headers.insert("x-request-id", "abc-123".parse().unwrap());
    })
    .on_response(|resp| {
        resp.headers_mut().insert("x-proxy", "aioduct".parse().unwrap());
    })
    .send()
    .await?;
# Ok(())
# }
```

## gRPC / h2c Forwarding

For gRPC or other HTTP/2 cleartext (h2c) upstreams, use `.h2c()` to force HTTP/2 prior knowledge on an individual forward without requiring `http2_prior_knowledge()` on the entire client:

```rust,no_run
# use aioduct::TokioClient;
# use aioduct::runtime::tokio_rt::TcpConnector;
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new(TcpConnector);

let grpc_req = http::Request::builder()
    .method("POST")
    .uri("/grpc.UserService/GetUser")
    .header("content-type", "application/grpc")
    .body(Full::new(Bytes::from("\0\0\0\0\x05hello")))
    .unwrap();

let resp = client
    .forward(grpc_req)
    .upstream("http://grpc-backend:50051".parse::<http::Uri>().unwrap())
    .h2c()
    .send()
    .await?;
# Ok(())
# }
```

### Adaptive h2c

When you don't know whether the upstream speaks h2c, use `.adaptive_h2c()`. On the first request to a given authority, it probes with an h2 prior knowledge handshake. If the upstream rejects it, the request falls back to HTTP/1.1 transparently. The result is cached per-authority so subsequent requests skip the probe:

```rust,no_run
# use aioduct::TokioClient;
# use aioduct::runtime::tokio_rt::TcpConnector;
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new(TcpConnector);

let req = http::Request::builder()
    .method("POST")
    .uri("/api/data")
    .body(Full::new(Bytes::new()))
    .unwrap();

// First request probes; subsequent requests use cached result
let resp = client
    .forward(req)
    .upstream("http://backend:8080".parse::<http::Uri>().unwrap())
    .adaptive_h2c()
    .send()
    .await?;
# Ok(())
# }
```

Configure the probe cache TTL (default 5 minutes) on the client:

```rust,no_run
# use aioduct::TokioClient;
# use aioduct::runtime::tokio_rt::TcpConnector;
# use std::time::Duration;
let client = TokioClient::builder(TcpConnector)
    .h2c_probe_ttl(Duration::from_secs(600))
    .build()?;
```

## What ForwardBuilder Does NOT Do

- **No body buffering** — the body streams through as-is
- **No middleware** — redirects, cookies, cache, and decompression are all bypassed
- **No WebSocket framing** — aioduct is transport-level; use a WS library for frame parsing
- **No bidirectional splice** — the caller is responsible for splicing `Upgraded` streams
- **No plaintext h2 by default** — HTTPS forwards negotiate HTTP/2 via TLS ALPN as usual; use `.h2c()` or `.adaptive_h2c()` when the upstream requires cleartext HTTP/2 (h2c)
