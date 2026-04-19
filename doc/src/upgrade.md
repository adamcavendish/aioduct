# HTTP Upgrade (WebSocket)

aioduct supports HTTP/1.1 protocol upgrades, commonly used for WebSocket connections. After a successful upgrade handshake, you get a bidirectional IO stream.

## Basic Usage

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

# async fn example() -> Result<(), aioduct::Error> {
let client = Client::<TokioRuntime>::new();

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

## How It Works

1. Call `.upgrade()` on the `RequestBuilder` to set the required headers (`Connection: Upgrade`, `Upgrade: websocket`) and force HTTP/1.1.
2. Send the request and check for a `101 Switching Protocols` response.
3. Call `.upgrade()` on the `Response` to consume it and obtain an `Upgraded` stream.
4. The connection is **not** returned to the pool — it's exclusively yours.

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

- HTTP upgrades only work over HTTP/1.1 (not HTTP/2 or HTTP/3)
- The `.upgrade()` method on `RequestBuilder` forces `Version::HTTP_11`
- After upgrade, the TCP connection is consumed — it won't be returned to the connection pool
- You can set additional WebSocket-specific headers (like `Sec-WebSocket-Key`) manually via `.header_str()`
