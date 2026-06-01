# API Reference

This page covers the main types and their methods. For full documentation, see `cargo doc --features tokio,rustls,rustls-ring,json`.

## Client Types

aioduct provides ergonomic type aliases for the most common configurations:

| Type Alias     | Expands To                                          | Runtime   |
|----------------|-----------------------------------------------------|-----------|
| `TokioClient`  | `HttpEngineSend<TokioRuntime, tokio_rt::TcpConnector>` | tokio     |
| `SmolClient`   | `HttpEngineSend<SmolRuntime, smol_rt::TcpConnector>`   | smol      |
| `CompioClient` | `HttpEngineLocal<CompioRuntime, compio_rt::TcpConnector>` | compio |

### Construction

```rust,no_run
use aioduct::TokioClient;
use std::time::Duration;

// Default configuration
let client = TokioClient::new();

// With rustls TLS (requires `rustls` and exactly one rustls provider)
let client = TokioClient::with_rustls();

// Custom configuration via builder
let client = TokioClient::builder()
    .timeout(Duration::from_secs(30))
    .connect_timeout(Duration::from_secs(10))
    .max_redirects(5)
    .pool_idle_timeout(Duration::from_secs(90))
    .pool_max_lifetime(Duration::from_secs(600))
    .pool_max_idle_per_host(10)
    .pool_max_active_streams_per_connection(100)
    .build()?;
```

Tokio, smol, and compio share the same native client builder surface. Blocking
clients wrap an already-configured async or local client, so pool, timeout,
TLS, proxy, retry, and HTTP/2 keep-alive settings are preserved by the wrapper.
Wasm and wasi-p2 clients use platform-managed transports, so connection
pooling, DNS, proxy, and TLS details are controlled by the browser or WASI host.

### HTTP Methods

| Method      | Description                    |
|-------------|--------------------------------|
| `get(url)`  | Start a GET request            |
| `head(url)` | Start a HEAD request           |
| `post(url)` | Start a POST request           |
| `put(url)`  | Start a PUT request            |
| `patch(url)`| Start a PATCH request          |
| `delete(url)` | Start a DELETE request       |
| `request(method, url)` | Start a request with any HTTP method |

All methods return `Result<RequestBuilderSend>` (or `Result<RequestBuilderLocal>` for `HttpEngineLocal`) — the URL is parsed immediately and invalid URLs produce an error.

### HttpEngineBuilder Options

`HttpEngineBuilder<R, C>` is returned by `TokioClient::builder(connector)` (and similarly for other client types).

| Method                  | Default      | Description                          |
|-------------------------|-------------|--------------------------------------|
| `timeout(Duration)`     | None        | Overall request deadline             |
| `connect_timeout(Duration)` | None   | Connection establishment timeout for TCP/proxy/TLS phases |
| `read_timeout(Duration)` | None      | Timeout between response body chunks |
| `tcp_keepalive(Duration)` | None     | Enable TCP keepalive with given interval |
| `local_address(IpAddr)`   | None     | Bind outgoing connections to a local IP  |
| `max_redirects(usize)`  | 10          | Maximum redirect hops (0 = disabled) |
| `referer(bool)`         | false       | Set `Referer` header on redirects    |
| `https_only(bool)`      | false       | Reject non-HTTPS URLs                |
| `pool_idle_timeout(Duration)` | 90s  | Idle connection lifetime             |
| `pool_max_lifetime(Duration)` | None | Maximum connection age before reuse stops |
| `pool_max_idle_per_host(usize)` | 10 | Max idle connections per origin      |
| `pool_max_active_streams_per_connection(usize)` | Unlimited | Max active HTTP/2 or HTTP/3 streams per pooled connection |
| `default_headers(HeaderMap)` | User-Agent | Headers applied to every request |
| `no_default_headers()`  | —           | Remove all default headers           |
| `tls(RustlsConnector)`  | None        | Custom rustls configuration, including caller-built ECH configs |
| `danger_accept_invalid_certs()` | —  | Accept any TLS certificate (INSECURE) |
| `no_decompression()`    | —           | Disable automatic response decompression |
| `system_proxy()`        | —           | Read proxy from HTTP_PROXY/HTTPS_PROXY/NO_PROXY env vars |
| `proxy_settings(ProxySettings)` | None | Fine-grained HTTP/HTTPS proxy with bypass rules |
| `http2(Http2Config)`    | None   | Configure HTTP/2 parameters (window sizes, keepalive, frame size) |
| `middleware(impl Middleware)` | None | Add a middleware layer that can inspect/modify requests and responses |
| `retry(RetryConfig)`    | None        | Default retry policy for all requests |
| `cookie_jar(CookieJar)` | None       | Enable automatic cookie management   |
| `rate_limiter(RateLimiter)` | None   | Token-bucket rate limiter for outgoing requests |
| `cache(HttpCache)`      | None        | Enable in-memory HTTP response caching |

## RequestBuilderSend / RequestBuilderLocal

Fluent builder for configuring a single request. `RequestBuilderSend` is returned by `HttpEngineSend` methods; `RequestBuilderLocal` is returned by `HttpEngineLocal` methods. Both implement `RequestBuilderExt`.

### Headers

```rust,no_run
# use aioduct::{TokioClient, HeaderMap};
# let client = TokioClient::new();
// Typed header
use http::header::{HeaderName, HeaderValue, ACCEPT};
let rb = client.get("http://example.com").unwrap()
    .header(ACCEPT, HeaderValue::from_static("application/json"));

// String header (fallible)
let rb = client.get("http://example.com").unwrap()
    .header_str("x-custom", "value").unwrap();

// Bulk headers
let mut headers = HeaderMap::new();
headers.insert("x-a", "1".parse().unwrap());
headers.insert("x-b", "2".parse().unwrap());
let rb = client.get("http://example.com").unwrap()
    .headers(headers);
```

### Authentication

```rust,no_run
# use aioduct::TokioClient;
# let client = TokioClient::new();
// Bearer token
let rb = client.get("http://example.com").unwrap()
    .bearer_auth("my-token");

// Basic auth
let rb = client.get("http://example.com").unwrap()
    .basic_auth("user", Some("password"));
```

### Body

```rust,no_run
# use aioduct::TokioClient;
# let client = TokioClient::new();
// Raw bytes
let rb = client.post("http://example.com").unwrap()
    .body("raw body content");

// URL-encoded form
let rb = client.post("http://example.com").unwrap()
    .form(&[("username", "admin"), ("password", "secret")]);

// Multipart form-data
// Generated boundaries are RFC 2046-safe and at most 70 bytes.
// let rb = client.post("http://example.com").unwrap()
//     .multipart(aioduct::Multipart::new().text("field", "value"));

// JSON (requires `json` feature)
// let rb = client.post("http://example.com").unwrap()
//     .json(&my_struct).unwrap();
```

### Query Parameters

```rust,no_run
# use aioduct::TokioClient;
# let client = TokioClient::new();
let rb = client.get("http://example.com/search").unwrap()
    .query(&[("q", "hello world"), ("page", "1")]);
// Sends: GET /search?q=hello%20world&page=1
```

### Other Options

```rust,no_run
# use aioduct::TokioClient;
# let client = TokioClient::new();
use std::time::Duration;

let rb = client.get("http://example.com").unwrap()
    .timeout(Duration::from_secs(5))     // per-request timeout
    .connect_timeout(Duration::from_secs(2)) // per-request connection timeout
    .version(http::Version::HTTP_11);    // force HTTP version

// HTTP upgrade (WebSocket)
let rb = client.get("http://example.com/ws").unwrap()
    .upgrade();  // sets Connection: Upgrade, Upgrade: websocket, HTTP/1.1
```

### Sending

```rust,no_run
# use aioduct::TokioClient;
# async fn example() -> Result<(), aioduct::Error> {
# let client = TokioClient::new();
let resp = client.get("http://example.com")?.send().await?;
# Ok(())
# }
```

## Error Handling

Async `send()` returns `SendError` on failure. It keeps the redacted request URL
next to the underlying `Error`, exposes helpers such as `is_timeout()`,
`is_connect()`, `is_status()`, and `status()`, and implements `source()` so
standard error-chain traversal works. `Error::root_cause()` and
`SendError::root_cause()` return the deepest source, and display output includes
hidden nested causes for boxed TLS or catch-all errors when the outer message
would otherwise omit the useful detail.

Timeout helpers distinguish the configured phases:

| Error | Typical source |
|-------|----------------|
| `Timeout` | Overall request deadline from `timeout()` |
| `ConnectTimeout` | Connection establishment deadline from `connect_timeout()` |
| `ReadTimeout` | Gap between response body chunks from `read_timeout()` |

## ResponseBodySend / ResponseBodyLocal

The response type returned after sending a request. `ResponseBodySend` is returned by `HttpEngineSend`; `ResponseBodyLocal` by `HttpEngineLocal`. Both implement `ResponseExt`.

### Inspecting

```rust,no_run
# use aioduct::TokioClient;
# async fn example() -> Result<(), aioduct::Error> {
# let client = TokioClient::new();
# let resp = client.get("http://example.com")?.send().await?;
let status = resp.status();           // StatusCode
let headers = resp.headers();         // &HeaderMap
let version = resp.version();         // Version
let length = resp.content_length();   // Option<u64>
let url = resp.url();                 // &Uri — final URL after redirects
# Ok(())
# }
```

### Error on Status

```rust,no_run
# use aioduct::TokioClient;
# async fn example() -> Result<(), aioduct::Error> {
# let client = TokioClient::new();
// Consume the response, returning Err for 4xx/5xx
let resp = client.get("http://example.com")?.send().await?
    .error_for_status()?;

// Non-consuming variant
let resp = client.get("http://example.com")?.send().await?;
resp.error_for_status_ref()?;
let text = resp.text().await?;
# Ok(())
# }
```

### Consuming the Body

```rust,no_run
# use aioduct::TokioClient;
# async fn example() -> Result<(), aioduct::Error> {
# let client = TokioClient::new();
// As bytes
let bytes = client.get("http://example.com")?.send().await?.bytes().await?;

// As string
let text = client.get("http://example.com")?.send().await?.text().await?;

// As JSON (requires `json` feature)
// let data: MyStruct = resp.json().await?;

// Raw hyper body
let body = client.get("http://example.com")?.send().await?.into_body();

// HTTP upgrade (WebSocket) — after 101 response
// let upgraded = resp.upgrade().await?;
# Ok(())
# }
```

## Blocking Client

With the `blocking` feature enabled, `BlockingTokioClient` wraps `TokioClient`
for synchronous callers. `BlockingResponse` exposes the same buffered consumers
as async responses and native response metadata accessors before body
consumption.

```rust,no_run
# #[cfg(all(feature = "blocking", feature = "tokio"))]
# fn example() -> Result<(), aioduct::Error> {
use aioduct::{BlockingTokioClient, TokioClient};

let client = BlockingTokioClient::new(TokioClient::new());
let mut resp = client.get("http://example.com/")?.send()?;
resp.headers_mut().insert("x-local", "yes".parse().unwrap());
let body = resp.bytes()?;
# Ok(())
# }
```

## Portable Traits

These traits provide a common interface across both `Send` and `Local` engine variants:

| Trait               | Description                                              |
|---------------------|----------------------------------------------------------|
| `HttpClient`        | Common client interface (`get`, `post`, etc.)            |
| `RequestBuilderExt` | Common request builder methods (`header`, `body`, etc.)  |
| `ResponseExt`       | Common response methods (`status`, `text`, `bytes`, etc.)|
| `ByteStreamExt`     | Streaming body helpers                                   |

Use these traits to write generic code that works with any aioduct client type.

## Redirects

aioduct follows redirects automatically (up to `max_redirects`, default 10):

| Status | Behavior                            |
|--------|-------------------------------------|
| 301    | Follow with GET, drop body + content headers |
| 302    | Follow with GET, drop body + content headers |
| 303    | Follow with GET, drop body + content headers |
| 307    | Follow with original method + body  |
| 308    | Follow with original method + body  |

Sensitive headers (`Authorization`, `Cookie`, `Proxy-Authorization`) are automatically stripped when redirecting to a different origin.

Disable with `.max_redirects(0)` on the builder.

## Request Lifecycle Observer

The `RequestObserver` trait provides real-time callbacks at every connection
phase transition with monotonic timestamps and diagnostic data. Use it for
per-request tracing, load testing metrics, or custom instrumentation.

```rust,no_run
use aioduct::{TokioClient, RequestObserver, RequestEvent, ConnectionEvent};

#[derive(Clone)]
struct MetricsObserver { /* atomic counters, channels, etc. */ }

impl RequestObserver for MetricsObserver {
    fn on_event(&self, event: &RequestEvent) {
        match &event.phase {
            RequestPhase::DnsResolved { addrs, duration } => { /* ... */ }
            RequestPhase::TlsHandshakeComplete { duration, alpn_protocol, .. } => { /* ... */ }
            RequestPhase::ResponseComplete { status, total_duration, .. } => { /* ... */ }
            _ => {}
        }
    }

    fn on_connection_event(&self, event: &ConnectionEvent) {
        // Connection-level lifecycle (pool checkin / close)
    }
}

let client = TokioClient::builder()
    .observer(MetricsObserver { /* ... */ })
    .build()?;
```

### RequestPhase Variants

| Phase | Key Fields | Fires When |
|-------|-----------|------------|
| `Started` | — | Request execution begins |
| `PoolCheckoutComplete` | `outcome`, `blocked_duration` | Pool lookup finishes |
| `DnsResolved` | `addrs`, `duration` | DNS resolution completes |
| `TcpConnected` | `remote_addr`, `duration`, `protocol` | TCP connection established |
| `TlsHandshakeComplete` | `duration`, `alpn_protocol`, `peer_certificate_der` | TLS negotiation done |
| `RequestSent` | `duration`, `headers` | Request fully sent to server |
| `ResponseStarted` | `waiting_duration` | TTFB — first response byte received |
| `ResponseComplete` | `status`, `protocol`, `total_duration` | Response headers complete |
| `Redirected` | `status`, `from`, `to` | A redirect was followed |
| `Retrying` | `reason`, `attempt`, `max_retries`, `backoff` | A retry is about to be attempted |
| `Failed` | `error`, `retry`, `elapsed` | Request failed with an error |
| `BytesTransferred` | `direction`, `chunk_bytes`, `cumulative_bytes` | Per-chunk (body streaming) |
| `TransferComplete` | `direction`, `total_bytes`, `throughput_bytes_per_sec` | Transfer in one direction finished |
| `TransferAborted` | `direction`, `bytes_transferred`, `error` | Transfer aborted mid-stream |
| `TrailersReceived` | `headers` | HTTP trailers received after body |

`RetryKind` (`None` / `StaleConnection` / `Explicit`) indicates whether and
how a failed request will be retried.

Phases that are skipped (DNS for pool hits, TLS for plain HTTP) simply don't fire.

## Trailers

HTTP trailers are optional header fields sent after the body in chunked
transfer encoding. They are exposed as body frames and via the observer:

```rust,no_run
use aioduct::TokioClient;
use http_body_util::BodyExt;

let resp = client
    .get("https://example.com/api")?
    .send()
    .await?;

// Trailers are available through the body frame stream
let mut body = resp.into_body();
while let Some(frame) = body.frame().await {
    let frame = frame?;
    if frame.is_trailers() {
        let trailers = frame.into_trailers().unwrap();
        for (name, value) in trailers.iter() {
            println!("{name}: {value:?}");
        }
    }
}
```

The `RequestObserver` also fires a `TrailersReceived` phase when trailers
arrive, with all trailer header fields.

## Error Types

```rust,no_run
use aioduct::Error;

// Error variants:
// Error::Http(_)         — http crate errors
// Error::Hyper(_)        — hyper protocol errors
// Error::Io(_)           — I/O errors
// Error::Tls(_)          — TLS errors
// Error::Pool(_)         — connection pool errors
// Error::Timeout         — request timed out
// Error::InvalidUrl(_)   — URL parse or scheme errors
// Error::Status(_)       — HTTP 4xx/5xx from error_for_status()
// Error::Other(_)        — other boxed errors
```

### Error Convenience Methods

| Method          | Description                                      |
|-----------------|--------------------------------------------------|
| `is_closed()`   | Returns `true` if the error is a closed connection |
| `is_timeout()`  | Returns `true` if the error is a timeout          |
| `is_connect()`  | Returns `true` if the error occurred during connect |
