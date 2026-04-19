# API Reference

This page covers the main types and their methods. For full documentation, see `cargo doc --features tokio,rustls,json`.

## Client

The main entry point. Generic over a `Runtime`.

### Construction

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

// Default configuration
let client = Client::<TokioRuntime>::new();

// With rustls TLS (requires `rustls` feature)
let client = Client::<TokioRuntime>::with_rustls();

// Custom configuration
let client = Client::<TokioRuntime>::builder()
    .timeout(std::time::Duration::from_secs(30))
    .max_redirects(5)
    .pool_idle_timeout(std::time::Duration::from_secs(90))
    .pool_max_idle_per_host(10)
    .build();
```

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

All methods return `Result<RequestBuilder>` — the URL is parsed immediately and invalid URLs produce an error.

### ClientBuilder Options

| Method                  | Default      | Description                          |
|-------------------------|-------------|--------------------------------------|
| `timeout(Duration)`     | None        | Default timeout for all requests     |
| `connect_timeout(Duration)` | None   | Timeout for TCP connect + TLS handshake |
| `tcp_keepalive(Duration)` | None     | Enable TCP keepalive with given interval |
| `local_address(IpAddr)`   | None     | Bind outgoing connections to a local IP  |
| `max_redirects(usize)`  | 10          | Maximum redirect hops (0 = disabled) |
| `https_only(bool)`      | false       | Reject non-HTTPS URLs                |
| `pool_idle_timeout(Duration)` | 90s  | Idle connection lifetime             |
| `pool_max_idle_per_host(usize)` | 10 | Max idle connections per origin      |
| `default_headers(HeaderMap)` | User-Agent | Headers applied to every request |
| `no_default_headers()`  | —           | Remove all default headers           |
| `tls(RustlsConnector)`  | None        | Custom TLS configuration             |
| `danger_accept_invalid_certs()` | —  | Accept any TLS certificate (INSECURE) |
| `no_decompression()`    | —           | Disable automatic response decompression |
| `system_proxy()`        | —           | Read proxy from HTTP_PROXY/HTTPS_PROXY/NO_PROXY env vars |
| `proxy_settings(ProxySettings)` | None | Fine-grained HTTP/HTTPS proxy with bypass rules |
| `resolver(impl Resolve)` | None   | Custom DNS resolver, overrides runtime default |
| `http2(Http2Config)`    | None   | Configure HTTP/2 parameters (window sizes, keepalive, frame size) |

## RequestBuilder

Fluent builder for configuring a single request.

### Headers

```rust,no_run
# use aioduct::{Client, HeaderMap};
# use aioduct::runtime::TokioRuntime;
# let client = Client::<TokioRuntime>::new();
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
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# let client = Client::<TokioRuntime>::new();
// Bearer token
let rb = client.get("http://example.com").unwrap()
    .bearer_auth("my-token");

// Basic auth
let rb = client.get("http://example.com").unwrap()
    .basic_auth("user", Some("password"));
```

### Body

```rust,no_run
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# let client = Client::<TokioRuntime>::new();
// Raw bytes
let rb = client.post("http://example.com").unwrap()
    .body("raw body content");

// URL-encoded form
let rb = client.post("http://example.com").unwrap()
    .form(&[("username", "admin"), ("password", "secret")]);

// JSON (requires `json` feature)
// let rb = client.post("http://example.com").unwrap()
//     .json(&my_struct).unwrap();
```

### Query Parameters

```rust,no_run
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# let client = Client::<TokioRuntime>::new();
let rb = client.get("http://example.com/search").unwrap()
    .query(&[("q", "hello world"), ("page", "1")]);
// Sends: GET /search?q=hello%20world&page=1
```

### Other Options

```rust,no_run
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# let client = Client::<TokioRuntime>::new();
use std::time::Duration;

let rb = client.get("http://example.com").unwrap()
    .timeout(Duration::from_secs(5))     // per-request timeout
    .version(http::Version::HTTP_11);    // force HTTP version
```

### Sending

```rust,no_run
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# async fn example() -> Result<(), aioduct::Error> {
# let client = Client::<TokioRuntime>::new();
let resp = client.get("http://example.com")?.send().await?;
# Ok(())
# }
```

## Response

### Inspecting

```rust,no_run
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# async fn example() -> Result<(), aioduct::Error> {
# let client = Client::<TokioRuntime>::new();
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
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# async fn example() -> Result<(), aioduct::Error> {
# let client = Client::<TokioRuntime>::new();
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
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
# async fn example() -> Result<(), aioduct::Error> {
# let client = Client::<TokioRuntime>::new();
// As bytes
let bytes = client.get("http://example.com")?.send().await?.bytes().await?;

// As string
let text = client.get("http://example.com")?.send().await?.text().await?;

// As JSON (requires `json` feature)
// let data: MyStruct = resp.json().await?;

// Raw hyper body
let body = client.get("http://example.com")?.send().await?.into_body();
# Ok(())
# }
```

## Redirects

aioduct follows redirects automatically (up to `max_redirects`, default 10):

| Status | Behavior                            |
|--------|-------------------------------------|
| 301    | Follow with GET, drop body          |
| 302    | Follow with GET, drop body          |
| 303    | Follow with GET, drop body          |
| 307    | Follow with original method + body  |
| 308    | Follow with original method + body  |

Sensitive headers (`Authorization`, `Cookie`, `Proxy-Authorization`) are automatically stripped when redirecting to a different origin.

Disable with `.max_redirects(0)` on the builder.

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
