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
    .pool_max_active_per_host(64)
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
| `base_url(&str)`        | None        | Base URL that relative request URLs resolve against (RFC 3986) |
| `timeout(Duration)`     | None        | Overall request deadline             |
| `connect_timeout(Duration)` | None   | Connection establishment timeout for TCP/proxy/TLS phases |
| `read_timeout(Duration)` | None      | Timeout between response body chunks |
| `write_timeout(Duration)` | None      | Timeout between request body chunks (upload) |
| `tcp_keepalive(Duration)` | None     | Enable TCP keepalive with given interval |
| `local_address(IpAddr)`   | None     | Bind outgoing connections to a local IP  |
| `address_family(AddressFamily)` | Any | Restrict/prefer IP family (Ipv4Only, Ipv6Only, PreferIpv4, PreferIpv6) for resolved connections |
| `max_redirects(usize)`  | 10          | Maximum redirect hops (0 = disabled) |
| `referer(bool)`         | false       | Set `Referer` header on redirects    |
| `https_only(bool)`      | false       | Reject non-HTTPS URLs                |
| `pool_idle_timeout(Duration)` | 90s  | Idle connection lifetime             |
| `pool_max_lifetime(Duration)` | None | Maximum connection age before reuse stops |
| `pool_max_idle_per_host(usize)` | 10 | Max idle connections per origin      |
| `pool_max_active_per_host(usize)` | Unlimited | Max checked-out handles and fresh connection attempts per pool key; 0 disables the cap |
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

> `base_url(&str)` returns `Result` because it validates the URL eagerly; the
> other setters return `Self`. When a base URL is set, a relative request URL
> (e.g. `client.get("users")`) resolves against it per RFC 3986, while an
> absolute request URL overrides it.

### Timeout Boundaries

| Timeout | Phase covered | Phase not covered |
|---------|---------------|-------------------|
| `timeout(Duration)` | One request attempt until `send()` returns, including redirects, response headers, and body upload | Retry backoff, later retry attempts, and response body reads after `send()` returns |
| `connect_timeout(Duration)` | TCP connection, proxy tunnel setup, and TLS handshake | Request upload and response body reads |
| `read_timeout(Duration)` | Gaps between response body chunks | Waiting for response headers and request upload |
| `write_timeout(Duration)` | Gaps while uploading request body chunks | Waiting for response headers and response body reads |

Per-request timeout setters override client defaults. `no_timeout()` disables a
client-level overall request timeout for one request, while phase-specific
timeouts still apply if configured on that request. When retries are enabled,
this timeout applies per attempt; it does not cap total wall-clock time across
backoff sleeps and later attempts. Use `read_timeout()` to bound stalled response
body reads after `send()` returns.

## RequestBuilderSend / RequestBuilderLocal

Fluent builder for configuring a single request. `RequestBuilderSend` is returned by `HttpEngineSend` methods; `RequestBuilderLocal` is returned by `HttpEngineLocal` methods. Both implement `RequestBuilderExt`. Fluent setters that cannot fail immediately record the error and return it from `build()` or `send()`.

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
//
// Custom boundary (validated, 1-70 RFC 2046 chars) and subtype:
// let form = aioduct::Multipart::new()
//     .with_boundary("WebKitFormBoundaryABC123")?   // Result
//     .subtype("mixed")?                            // multipart/mixed
//     .text("field", "value");

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
    .read_timeout(Duration::from_secs(30))   // per-request response read-gap timeout
    .write_timeout(Duration::from_secs(10))  // per-request upload timeout
    .no_decompression()                      // per-request: skip Accept-Encoding + decoding
    .version(http::Version::HTTP_11);    // force HTTP version

// HTTP upgrade (WebSocket)
let rb = client.get("http://example.com/ws").unwrap()
    .upgrade();  // sets Connection: Upgrade, Upgrade: websocket, HTTP/1.1
```

### Inspecting a builder

Read accessors let you inspect a configured request before sending it (e.g. for
logging, signing, or library wrappers). `method_ref()` returns the method,
`url()` the resolved URL, and `headers_ref()` the headers added so far (client
default headers are merged at send time, so they are not reflected here). Call
`build()` to get the full `http::Request` without sending.

```rust,no_run
# use aioduct::TokioClient;
# let client = TokioClient::new();
let rb = client.post("http://example.com/api").unwrap()
    .header(http::header::ACCEPT, http::HeaderValue::from_static("application/json"));
assert_eq!(rb.method_ref(), &http::Method::POST);
assert_eq!(rb.url().to_string(), "http://example.com/api");
assert!(rb.headers_ref().contains_key(http::header::ACCEPT));
```

The builders are `#[must_use]`: a `RequestBuilder` that is never sent or built,
or a `ClientBuilder` that is never built, produces a compiler warning.

### HTTP Message Signatures

`MessageSignatureConfig` builds RFC 9421 request signature bases and formats the
`Signature-Input` / `Signature` headers from caller-provided signature bytes. The
helpers are portable and do not choose a cryptographic algorithm.

```rust,no_run
# use aioduct::{MessageSignatureComponent, MessageSignatureConfig};
# use http::{HeaderMap, Method, Uri};
# fn example() -> Result<(), Box<dyn std::error::Error>> {
let target_uri: Uri = "https://example.com/api".parse()?;
let request_target: Uri = "/api".parse()?;
let headers = HeaderMap::new();

let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::Method)
    .component(MessageSignatureComponent::Authority)
    .created(1_618_884_473)
    .key_id("test-key");

let base = config.signature_base(&Method::GET, &target_uri, &request_target, &headers)?;
let signature = sign_with_your_key(base.as_bytes());
let signature_headers = config.headers_from_signature(signature)?;
# Ok(())
# }
# fn sign_with_your_key(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

Native clients can also sign each finalized request attempt automatically:

```rust,no_run
# use aioduct::{MessageSignatureComponent, MessageSignatureConfig, TokioClient};
# fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::Method)
    .component(MessageSignatureComponent::Authority)
    .key_id("test-key");

let client = TokioClient::builder()
    .message_signature(config, |base: &[u8]| Ok(sign_with_your_key(base)))
    .build()?;
# let _ = client;
# Ok(())
# }
# fn sign_with_your_key(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

Automatic signing runs after default headers, cookies, cache validators,
middleware, digest-auth retry headers, and framing cleanup. When configured, it
replaces `Signature-Input` and `Signature` on every native dispatch attempt.
Forwarding automatic signing is Future Work.

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
| `WriteTimeout` | Gap between request body chunks from `write_timeout()` |

### Reading the body of an error response

`error_for_status()` turns a 4xx/5xx into `Error::Status(code)` and does not
capture the body, so the error value stays cheap. When you need the error
payload (an API's JSON error message, for example), read it as a separate stage:
`status()` is a synchronous, non-consuming check, so gate on it and then read the
body yourself.

```rust,no_run
# use aioduct::TokioClient;
# async fn example() -> Result<(), aioduct::Error> {
# let client = TokioClient::new();
let resp = client.get("http://example.com/api")?.send().await?;
if resp.status().is_client_error() || resp.status().is_server_error() {
    let status = resp.status();
    let body = resp.text().await?; // read the error body as its own stage
    eprintln!("server said {status}: {body}");
    return Ok(());
}
let body = resp.text().await?; // success path
# let _ = body;
# Ok(())
# }
```

`error_for_status_ref()` borrows instead of consuming, so you can check status
without giving up ownership of the response, then read the body on either branch.
Status and body are intentionally decoupled, and reading the body consumes the
response (no implicit buffering), matching reqwest's and aiohttp's model.

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
consumption. Blocking request builders forward the common buffered request-body
and per-request behavior controls, including `body()`, `form()`, `timeout()`,
`read_timeout()`, `connect_timeout()`, `no_decompression()`, `query()`, and
`version()`.

```rust,no_run
# #[cfg(all(feature = "blocking", feature = "tokio"))]
# fn example() -> Result<(), aioduct::Error> {
use aioduct::{BlockingTokioClient, TokioClient};

let client = BlockingTokioClient::new(TokioClient::new());
let mut resp = client.post("http://example.com/")?
    .form(&[("name", "alice")])
    .no_decompression()
    .send()?;
resp.headers_mut().insert("x-local", "yes".parse().unwrap());
let body = resp.bytes()?;
# Ok(())
# }
```

## Portable Traits

These traits provide a common interface across client types. Implementations must either apply each request-builder option or fail explicitly when the request is sent; unsupported options are not silently ignored.

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
        // Connection-level metrics fire when a connection is checked back into
        // the pool or closed. The metrics include protocol, remote address,
        // approximate bytes sent/received, connection age, requests served, and
        // whether the connection was closed instead of returned to the pool.
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
transfer encoding. They are available through three channels:

### Via the bytes stream

The simplest approach: drain the body with `into_bytes_stream()` and call
`trailers()` once the stream is exhausted. Trailers are captured
automatically as non-data frames are consumed.

```rust,no_run
use aioduct::TokioClient;

let resp = client
    .get("https://example.com/api")?
    .send()
    .await?;

let mut stream = resp.into_bytes_stream();
while let Some(chunk) = stream.next().await {
    let _bytes = chunk?;
    // process body data …
}

// Trailers are available after the stream is fully consumed
if let Some(trailers) = stream.trailers() {
    for (name, value) in trailers.iter() {
        println!("trailer {name}: {value:?}");
    }
}
```

### Via the raw body frame stream

For lower-level control, iterate the body frames directly:

```rust,no_run
use aioduct::TokioClient;
use http_body_util::BodyExt;

let resp = client
    .get("https://example.com/api")?
    .send()
    .await?;

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

### Via the request observer

The `RequestObserver` fires a `TrailersReceived` phase when trailers
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
// PoolLimitKind          — what pool limit was reached
// Error::Timeout         — request timed out
// Error::ConnectTimeout  — connection establishment timed out
// Error::ReadTimeout     — reading response timed out
// Error::WriteTimeout    — writing request body timed out
// Error::InvalidUrl(_)   — URL parse or scheme errors
// Error::InvalidHeader(_) — header name or value errors
// Error::Unsupported(_)  — runtime or transport does not support an option
// Error::Status(_)       — HTTP 4xx/5xx from error_for_status()
// Error::Other(_)        — other boxed errors
```

### Error Convenience Methods

| Method          | Description                                      |
|-----------------|--------------------------------------------------|
| `is_closed()`   | Returns `true` if the error is a closed connection |
| `is_timeout()`  | Returns `true` if the error is a timeout          |
| `is_write_timeout()` | Returns `true` if the error is an upload timeout |
| `is_connect()`  | Returns `true` if the error occurred during connect |
| `is_pool_limit()` | Returns `true` if the error is a pool limit (client-side backpressure) |
