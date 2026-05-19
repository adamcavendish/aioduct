# HTTP Upgrade (WebSocket)

aioduct supports HTTP/1.1 protocol upgrades (101 Switching Protocols) and HTTP/2 extended CONNECT (RFC 8441), commonly used for WebSocket connections. After a successful upgrade handshake, you get a bidirectional IO stream.

## Basic Usage (HTTP/1.1)

```rust,no_run
use aioduct::TokioClient;
use aioduct::runtime::tokio_rt::TcpConnector;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::new(TcpConnector);

let resp = client
    .get("http://example.com/ws")?
    .upgrade()  // sets Connection: Upgrade + Upgrade: websocket + HTTP/1.1
    .send()
    .await?;

assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);

let upgraded = resp.upgrade().await?;
// `upgraded` implements hyper's Read + Write traits
// With the `tokio` feature, it also implements tokio::io::AsyncRead + AsyncWrite
# Ok(())
# }
```

## HTTP/2 Extended CONNECT (RFC 8441)

For HTTP/2 upstreams that support the extended CONNECT protocol, use `Protocol` to signal the desired sub-protocol:

```rust,no_run
use aioduct::{TokioClient, Protocol};
use aioduct::runtime::tokio_rt::TcpConnector;

# async fn example() -> Result<(), aioduct::Error> {
let client = TokioClient::builder(TcpConnector)
    .http2_prior_knowledge()
    .build()?;

let mut req = client
    .get("http://example.com/ws/chat")?
    .build();
*req.method_mut() = http::Method::CONNECT;
req.extensions_mut().insert(Protocol::from_static("websocket"));

let resp = client.execute(req).await?;
assert_eq!(resp.status(), http::StatusCode::OK);

let upgraded = resp.upgrade().await?;
// Bidirectional tunnel over the H2 stream
# Ok(())
# }
```

For proxy/gateway use cases, see [Request Forwarding](request_forwarding.md) which auto-detects both upgrade mechanisms.

## How It Works

1. **HTTP/1.1**: Call `.upgrade()` on the `RequestBuilder` to set the required headers (`Connection: Upgrade`, `Upgrade: websocket`) and force HTTP/1.1.
2. **HTTP/2**: Insert a `Protocol` extension into the request and use `CONNECT` method. The server must have `SETTINGS_ENABLE_CONNECT_PROTOCOL` enabled.
3. Send the request and check for `101` (H1) or `200` (H2 CONNECT).
4. Call `.upgrade()` on the `Response` to consume it and obtain an `Upgraded` stream.
5. The connection is **not** returned to the pool — it's exclusively yours.

## The Upgraded Type

`Upgraded` is a bidirectional IO stream:

- Implements `hyper::rt::Read` and `hyper::rt::Write` (always available)
- Implements `tokio::io::AsyncRead` and `tokio::io::AsyncWrite` (when the `tokio` feature is enabled)
- Can be converted to the underlying `hyper::upgrade::Upgraded` via `.into_inner()`
- Can be constructed from `hyper::upgrade::Upgraded` via `Upgraded::from()`

## Using with WebSocket Libraries

Pass the `Upgraded` stream to your WebSocket library of choice. For example, with `tokio-tungstenite`:

```rust,ignore
let upgraded = resp.upgrade().await?;
let ws_stream = tokio_tungstenite::WebSocketStream::from_raw_socket(
    upgraded,
    tokio_tungstenite::tungstenite::protocol::Role::Client,
    None,
).await;
```

## Notes

- HTTP/1.1 upgrades use `Connection: Upgrade` + `Upgrade: websocket` headers → 101
- HTTP/2 extended CONNECT uses `CONNECT` method + `:protocol` pseudo-header → 200
- After upgrade, the connection/stream is consumed — it won't be returned to the pool
- You can set additional WebSocket-specific headers (like `Sec-WebSocket-Key`) manually via `.header_str()`

