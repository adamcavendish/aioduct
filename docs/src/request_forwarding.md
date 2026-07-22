# Request Forwarding

aioduct includes a built-in request forwarding builder for reverse proxy and API gateway use cases. It strips hop-by-hop headers, rewrites the URI to target an upstream, streams the body without buffering, and bypasses all client middleware (redirects, cookies, cache, decompression).

## Basic Forwarding

```rust,no_run
use aioduct::TokioClient;
use bytes::Bytes;
use http_body_util::Full;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();

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
| `.downstream_target_uri(uri)` | Full downstream URI for related-request response signatures |
| `.response_content_digest(max_bytes)` | Buffer the downstream response up to a cap and insert SHA-256 `Content-Digest` |
| `.response_message_signature(config, signer)` | Sign the downstream response with a sync RFC 9421 signer |
| `.response_message_signature_async(config, signer)` | Sign the downstream response with a send-runtime async signer (send builders) |
| `.response_message_signature_async_local(config, signer)` | Sign the downstream response with a local-runtime async signer (local builders) |
| `.h2c()` | Force HTTP/2 prior knowledge (h2c) on this forward |
| `.adaptive_h2c()` | Probe h2c, fall back to h1; result cached per effective route and forced address |
| `.upgrade()` | Force upgrade header preservation (usually auto-detected) |

## Hop-by-Hop Header Stripping

`ForwardBuilderSend` automatically strips hop-by-hop fields from both the
incoming request and the upstream response:

- `Connection`
- `Keep-Alive`
- `Proxy-Authenticate`
- `Proxy-Authorization`
- `Proxy-Connection`
- `Transfer-Encoding`

Use `.forward_header(name)` to preserve ordinary headers through the upstream
rewrite. It does not override protocol safety rules for hop-by-hop fields.

Protocol-specific fields are handled after the final request hook and selected
upstream protocol are known. When HTTP/1.1 trailer negotiation applies, aioduct
regenerates canonical `Connection: TE` and `TE: trailers` fields; HTTP/1.0
removes `TE`. HTTP/2 and HTTP/3 may retain only canonical `TE: trailers`.
`Upgrade` is restored only for a validated HTTP/1.1 upgrade, and
`HTTP2-Settings` only for a valid h2c upgrade. Fields named by `Connection` are
removed unless that upgrade policy explicitly restores them.

`Trailer` declarations are preserved for end-to-stream metadata, but names
that are forbidden in trailer fields are removed from the declaration. Actual
trailer frames are sanitized on both request and response bodies. Framing,
routing, authentication, request-control, response-control, and payload
interpretation fields such as `Content-Length`, `Host`, `Authorization`,
`Set-Cookie`, and `Content-Type` are never forwarded as trailers. Extension
metadata such as `X-Upload-Checksum` remains eligible.

Actual HTTP/3 request and response trailer frames currently fail closed with
`Error::Unsupported`, even when their fields would otherwise be eligible.

## WebSocket / HTTP Upgrade Forwarding

Upgrade requests are auto-detected and handled correctly:

### HTTP/1.1 Upgrade

When `Connection: Upgrade` is present, `ForwardBuilderSend`:
- Preserves `Connection` and `Upgrade` headers through hop-by-hop stripping
- Forces HTTP/1.1 on the upstream connection
- Sanitizes the `101` response, then restores only the validated
  `Connection: upgrade` and `Upgrade` fields required for the tunnel

```rust,no_run
use aioduct::TokioClient;
use bytes::Bytes;
use http_body_util::Full;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();

let ws_req = http::Request::builder()
    .method("GET")
    .uri("/ws/chat")
    .header("host", "proxy.example")
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

When the request method is `CONNECT` and a `Protocol` extension is present, `ForwardBuilderSend`:
- Forces HTTP/2 on the upstream connection
- Uses the full URI (not path-only) so hyper generates correct pseudo-headers
- Validates and sanitizes response headers and trailers before tunnel handoff

```rust,no_run
use aioduct::{TokioClient, Protocol};
use bytes::Bytes;
use http_body_util::Full;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::builder()
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
    .h2c()
    .send()
    .await?;

assert_eq!(resp.status(), http::StatusCode::OK);

let mut upstream_io = resp.upgrade().await?;
// Bidirectional tunnel is ready
# Ok(())
# }
```

HTTP/2 tunnel handoff currently requires status `200 OK` for both ordinary and
extended CONNECT. Hyper 1.10 does not expose the bidirectional stream for other
successful 2xx statuses, so aioduct returns an explicit unsupported error
instead of exposing a one-way or false tunnel. HTTP/1.1 CONNECT continues to
accept the full successful 2xx range.

## Hooks

Use `on_request` and `on_response` for transformations not covered by other builder methods:

```rust,no_run
# use aioduct::TokioClient;
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
# let client = TokioClient::new();
# let incoming_req = http::Request::builder().uri("/test").header("host", "proxy.example").body(Full::new(Bytes::new())).unwrap();
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

## Response Message Signatures

Forward builders can sign the response that a gateway returns downstream with RFC
9421 HTTP Message Signatures. Response signing runs after upstream response
hop-by-hop headers are stripped and after `on_response` runs, then strips
hop-by-hop headers again before building the signature base. This means response
mutations from the hook are covered, but `Connection`-listed fields are not.

```rust,no_run
# use aioduct::{MessageSignatureComponent, MessageSignatureConfig, TokioClient};
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();
let incoming_req = http::Request::builder()
    .method("GET")
    .uri("/api/users")
    .header("host", "gateway.example.com")
    .body(Full::new(Bytes::new()))
    .unwrap();

let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::status())
    .component(MessageSignatureComponent::header(
        http::header::HeaderName::from_static("x-gateway"),
    ));

let resp = client
    .forward(incoming_req)
    .upstream("http://backend:8080".parse::<http::Uri>().unwrap())
    .on_response(|resp| {
        resp.headers_mut().insert("x-gateway", "aioduct".parse().unwrap());
    })
    .response_message_signature(config, |base: &[u8]| Ok(sign_with_your_key(base)))
    .send()
    .await?;
# let _ = resp;
# Ok(())
# }
# fn sign_with_your_key(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

When the response signature covers related request components with `;req`, the
request data comes from the inbound request snapshot, not the rewritten upstream
request. If the inbound request uses origin-form and the signature covers
related-request `@scheme`, `@authority`, or `@target-uri`, provide the full
downstream URI explicitly:

```rust,no_run
# use aioduct::{MessageSignatureComponent, MessageSignatureConfig, TokioClient};
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();
let incoming_req = http::Request::builder()
    .method("GET")
    .uri("/api/users?limit=10")
    .header("host", "gateway.example.com")
    .body(Full::new(Bytes::new()))
    .unwrap();

let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::status())
    .component(MessageSignatureComponent::target_uri().related_request())
    .component(MessageSignatureComponent::authority().related_request());

let resp = client
    .forward(incoming_req)
    .upstream("http://backend:8080".parse::<http::Uri>().unwrap())
    .downstream_target_uri("https://gateway.example.com/api/users?limit=10")
    .response_message_signature(config, |base: &[u8]| Ok(sign_with_your_key(base)))
    .send()
    .await?;
# let _ = resp;
# Ok(())
# }
# fn sign_with_your_key(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

Use `.response_content_digest(max_bytes)` when the gateway should add a
downstream response body digest before response signing:

```rust,no_run
# use aioduct::{MessageSignatureComponent, MessageSignatureConfig, TokioClient};
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();
let incoming_req = http::Request::builder()
    .method("GET")
    .uri("/api/report")
    .body(Full::new(Bytes::new()))
    .unwrap();

let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::status())
    .component(MessageSignatureComponent::header(
        http::header::HeaderName::from_static(aioduct::CONTENT_DIGEST),
    ));

let resp = client
    .forward(incoming_req)
    .upstream("http://backend:8080".parse::<http::Uri>().unwrap())
    .response_content_digest(64 * 1024)
    .response_message_signature(config, |base: &[u8]| Ok(sign_with_your_key(base)))
    .send()
    .await?;
# let _ = resp;
# Ok(())
# }
# fn sign_with_your_key(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

Response digest generation runs after upstream response hop-by-hop cleanup and
`on_response`, then before response signing. It preserves an existing
`Content-Digest` without buffering. If the response body exceeds `max_bytes`, the
forward returns an error instead of producing an undigested response. Bodyless
responses such as `HEAD`, `204`, `205`, and `304` are not assigned synthesized
digest fields.

Automatic response finalization is fail-closed: signer failures, malformed
existing `Signature-Input` / `Signature` dictionaries, unsupported trailer
components, response digest bodies over the configured cap, `CONNECT`, known
upgrade requests, and HTTP/1.1 `101 Switching Protocols` responses return an
error instead of an unsigned or undigested response.

## gRPC / h2c Forwarding

For gRPC or other HTTP/2 cleartext (h2c) upstreams, use `.h2c()` to force HTTP/2 prior knowledge on an individual forward without requiring `http2_prior_knowledge()` on the entire client:

```rust,no_run
# use aioduct::TokioClient;
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();

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

When you don't know whether the upstream speaks h2c, use `.adaptive_h2c()`.
On the first request for an unknown effective route and endpoint, it probes
with an h2 prior-knowledge handshake. If the upstream rejects it, the request
falls back to HTTP/1.1 transparently. The cache key includes the origin scheme,
host, effective port, complete proxy route, and any forced transport address.
The same authority reached directly, through a proxy, or through different
forced addresses is therefore probed independently:

```rust,no_run
# use aioduct::TokioClient;
# use bytes::Bytes;
# use http_body_util::Full;
# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new();

let req = http::Request::builder()
    .method("POST")
    .uri("/api/data")
    .body(Full::new(Bytes::new()))
    .unwrap();

// First request probes; subsequent requests on this route use the cached result
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
# use std::time::Duration;
let client = TokioClient::builder()
    .h2c_probe_ttl(Duration::from_secs(600))
    .build()?;
```

## What ForwardBuilderSend Does NOT Do

- **No request body buffering** — the incoming request body streams through as-is
- **No middleware** — redirects, cookies, cache, and decompression are all bypassed
- **No streaming response digesting** — response `Content-Digest` generation uses bounded full-body buffering, not trailers
- **No automatic trailer finalization** — response signing does not synthesize digest or signature trailers while streaming downstream bodies
- **No WebSocket framing** — aioduct is transport-level; use a WS library for frame parsing
- **No bidirectional splice** — the caller is responsible for splicing upgrade streams
- **No plaintext h2 by default** — HTTPS forwards negotiate HTTP/2 via TLS ALPN as usual; use `.h2c()` or `.adaptive_h2c()` when the upstream requires cleartext HTTP/2 (h2c)
