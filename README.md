# aioduct

[![Crates.io](https://img.shields.io/crates/v/aioduct.svg)](https://crates.io/crates/aioduct)
[![docs.rs](https://docs.rs/aioduct/badge.svg)](https://docs.rs/aioduct)
[![CI](https://github.com/adamcavendish/aioduct/actions/workflows/ci.yml/badge.svg)](https://github.com/adamcavendish/aioduct/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV: 1.85](https://img.shields.io/badge/MSRV-1.85-brightgreen.svg)](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html)

Async-native Rust HTTP client built directly on **hyper 1.x** — no hyper-util, no legacy APIs.

[Documentation](https://adamcavendish.github.io/aioduct/) | [API Reference](https://docs.rs/aioduct) | [Crates.io](https://crates.io/crates/aioduct)

## Why aioduct?

- **reqwest** depends on hyper-util's `legacy::Client`, wrapping hyper 0.x-style patterns over hyper 1.x with years of backwards-compatibility baggage.
- **hyper-util** labels its own client as "legacy" — the hyper team acknowledges it's not the long-term answer.
- **hyper 1.x** provides clean connection-level primitives, but no production client uses them directly.

aioduct uses hyper 1.x **the way it was intended** — as a protocol engine you drive yourself, with your own connection pool, TLS, and runtime integration.

## Features

- **No hyper-util** — custom IO adapters and executor directly against `hyper::rt` traits
- **Multi-runtime** — tokio, smol, and compio (io_uring) via feature flags; WASM/browser support
- **rustls TLS** — async handshake with ALPN-based HTTP/1.1 and HTTP/2 negotiation
- **Connection pooling** — keyed by (scheme, authority) with idle timeout and per-host limits
- **Redirect following** — RFC-compliant handling of 301/302/303/307/308 with sensitive header stripping and content header removal
- **Cookie jar** — automatic cookie storage, domain/path/subdomain matching, Max-Age and Expires expiration, Secure flag enforcement
- **Timeouts** — per-request, client-level, connect, and read timeouts
- **Retry** — configurable exponential backoff with retry budgets, per-request or client-level
- **Decompression** — automatic gzip, brotli, zstd, deflate response decompression
- **Proxy** — HTTP CONNECT tunneling, SOCKS4/SOCKS4a, SOCKS5, system proxy detection (HTTP_PROXY/HTTPS_PROXY/NO_PROXY)
- **Middleware** — pluggable request/response interceptors via trait or closure
- **Rate limiting** — token-bucket rate limiter for outgoing requests
- **Caching** — in-memory HTTP cache with configurable max entries
- **SSE** — Server-Sent Events stream parsing for LLM APIs
- **Multipart** — `multipart/form-data` uploads with text fields and file parts
- **Streaming** — chunked downloads and streaming uploads without buffering
- **Chunk download** — parallel HTTP Range requests for large files
- **HTTP upgrade** — WebSocket and other protocol upgrades via HTTP/1.1 101
- **Blocking client** — synchronous wrapper for non-async contexts (requires tokio)
- **Custom DNS** — pluggable resolver via the `Resolve` trait; hickory-dns integration
- **HTTP/2 tuning** — configurable window sizes, frame size, adaptive window, keepalive PINGs
- **TCP keepalive** — configurable keepalive interval for long-lived connections
- **Local address binding** — bind outgoing connections to a specific local IP
- **JSON** — optional `json` feature for request/response serialization
- **Happy Eyeballs** — RFC 6555 connection racing, interleaves IPv6/IPv4 with 250ms stagger
- **Digest auth** — automatic HTTP Digest authentication with 401 retry (RFC 7616, MD5)
- **Bandwidth limiter** — token-bucket byte-rate throttle for download speed limiting
- **Netrc** — `.netrc` file parser and middleware for automatic credential injection
- **Auth helpers** — bearer token, basic auth
- **Form data** — URL-encoded form bodies
- **Query parameters** — with percent-encoding
- **Default headers** — automatic User-Agent, configurable defaults
- **Observability** — optional tracing spans and OpenTelemetry middleware
- **Tower integration** — use aioduct as a tower `Service`

## Quick Start

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio"] }
```

```rust
use aioduct::{Client, StatusCode};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    let resp = client.get("http://httpbin.org/get")?
        .send()
        .await?;

    assert_eq!(resp.status(), StatusCode::OK);
    println!("{}", resp.text().await?);
    Ok(())
}
```

## HTTPS

Enable the `rustls` feature:

```toml
aioduct = { version = "0.1", features = ["tokio", "rustls"] }
```

```rust
let client = Client::<TokioRuntime>::with_rustls();
let resp = client.get("https://httpbin.org/get")?.send().await?;
```

## Feature Flags

| Feature   | Description                             | Stability    |
|-----------|----------------------------------------|--------------|
| `tokio`   | Tokio async runtime                    | Stable       |
| `smol`    | Smol async runtime                     | Stable       |
| `compio`  | Compio runtime (io_uring / IOCP)       | Experimental |
| `wasm`    | Browser/WASM runtime via web-sys       | Experimental |
| `rustls`  | TLS via rustls (required for HTTPS)    | Stable       |
| `rustls-native-roots` | Use OS certificate store instead of webpki-roots | Stable |
| `json`    | JSON request/response with serde       | Stable       |
| `charset` | Charset decoding via encoding_rs       | Stable       |
| `gzip`    | Gzip response decompression            | Stable       |
| `deflate` | Deflate response decompression         | Stable       |
| `brotli`  | Brotli response decompression          | Stable       |
| `zstd`    | Zstd response decompression            | Stable       |
| `blocking`| Synchronous blocking client (requires tokio) | Stable |
| `hickory-dns` | DNS via hickory-resolver (requires tokio) | Stable |
| `tower`   | Tower `Service` and `Layer` integration | Stable      |
| `tracing` | Tracing spans for requests             | Stable       |
| `otel`    | OpenTelemetry middleware               | Stable       |
| `http3`   | HTTP/3 via h3 + h3-quinn              | Experimental |

At least one runtime feature must be enabled or compilation will fail.

## Examples

### JSON

```rust
// Requires features = ["tokio", "json"]
let resp = client.post("https://api.example.com/users")?
    .json(&serde_json::json!({"name": "Alice"}))?
    .send()
    .await?;

let user: User = resp.json().await?;
```

### Form Data

```rust
let resp = client.post("https://example.com/login")?
    .form(&[("username", "admin"), ("password", "secret")])
    .send()
    .await?;
```

### Authentication

```rust
// Bearer token
let resp = client.get("https://api.example.com/me")?
    .bearer_auth("my-token")
    .send()
    .await?;

// Basic auth
let resp = client.get("https://example.com/protected")?
    .basic_auth("user", Some("pass"))
    .send()
    .await?;
```

### Query Parameters

```rust
let resp = client.get("https://example.com/search")?
    .query(&[("q", "hello world"), ("page", "1")])
    .send()
    .await?;
// GET /search?q=hello%20world&page=1
```

### Client Configuration

```rust
use std::time::Duration;

let client = Client::<TokioRuntime>::builder()
    .timeout(Duration::from_secs(30))
    .max_redirects(5)
    .pool_idle_timeout(Duration::from_secs(90))
    .pool_max_idle_per_host(10)
    .tcp_keepalive(Duration::from_secs(60))
    .local_address("192.168.1.100".parse().unwrap())
    .build();
```

### SOCKS5 Proxy

```rust
use aioduct::ProxyConfig;

let client = Client::<TokioRuntime>::builder()
    .proxy(ProxyConfig::socks5("socks5://proxy.example.com:1080").unwrap())
    .build();

// With authentication
let client = Client::<TokioRuntime>::builder()
    .proxy(
        ProxyConfig::socks5("socks5://proxy.example.com:1080")
            .unwrap()
            .basic_auth("user", "pass"),
    )
    .build();
```

### HTTP/2 Tuning

```rust
use aioduct::Http2Config;

let client = Client::<TokioRuntime>::builder()
    .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
    .http2(
        Http2Config::new()
            .initial_stream_window_size(2 * 1024 * 1024)
            .adaptive_window(true)
            .keep_alive_interval(Duration::from_secs(20))
            .keep_alive_while_idle(true),
    )
    .build();
```

### Smol Runtime

```rust
use aioduct::Client;
use aioduct::runtime::SmolRuntime;

smol::block_on(async {
    let client = Client::<SmolRuntime>::new();
    let resp = client.get("http://httpbin.org/get")?
        .send()
        .await?;
    println!("{}", resp.text().await?);
    Ok::<_, aioduct::Error>(())
});
```

## CLI Tools

The workspace includes two CLI tools built on aioduct:

### aioduct-aria

An aria2-inspired parallel download tool. Splits large files into segments and downloads them concurrently using HTTP Range requests.

```sh
# Download with 8 segments
aioduct-aria -s 8 https://example.com/large-file.tar.gz

# Resume an interrupted download
aioduct-aria -c https://example.com/large-file.tar.gz
```

### aioduct-curl

A curl-inspired HTTP tool with familiar flags.

```sh
# GET request
aioduct-curl https://httpbin.org/get

# POST with JSON body
aioduct-curl -X POST -d '{"key":"val"}' -H 'Content-Type: application/json' https://httpbin.org/post

# Follow redirects, basic auth, save to file
aioduct-curl -L -u user:pass -o output.html https://example.com
```

Both tools are workspace members (`publish = false`) and serve as real-world integration examples.

## Architecture

```
Client<R: Runtime>
  ├── RequestBuilder      ← fluent API (headers, body, auth, query, timeout)
  ├── ConnectionPool<R>   ← keyed by (scheme, authority), idle eviction
  ├── TLS (rustls)        ← async handshake, ALPN → h1/h2
  └── Runtime trait       ← TcpStream, Sleep, spawn, resolve
       ├── TokioRuntime
       ├── SmolRuntime
       └── CompioRuntime
```

The `Runtime` trait abstracts over async runtimes:

```rust
pub trait Runtime: Send + Sync + 'static {
    type TcpStream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;
    type Sleep: Future<Output = ()> + Send;

    async fn connect(addr: SocketAddr) -> io::Result<Self::TcpStream>;
    async fn resolve(host: &str, port: u16) -> io::Result<SocketAddr>;
    fn sleep(duration: Duration) -> Self::Sleep;
    fn spawn<F: Future<Output = ()> + Send + 'static>(future: F);
}
```

## Comparison

| | reqwest | aioduct |
|---|---|---|
| hyper | 1.x via hyper-util legacy | 1.x direct |
| hyper-util | Required | Not used |
| Runtime | tokio only | tokio / smol / compio / wasm |
| TLS | rustls or native-tls | rustls |
| HTTP/3 | Experimental | Experimental |
| io_uring | No | Via compio |
| Connection pool | hyper-util legacy | Custom h1/h2/h3 |
| Cookie jar | Yes | Yes |
| SSE streaming | No (manual) | Built-in |
| Rate limiting | No | Built-in |
| HTTP caching | No | Built-in |
| Middleware | Via tower | Built-in + tower |
| Happy Eyeballs | No | RFC 6555 |
| Digest auth | No | Built-in |
| Bandwidth limiter | No | Built-in |
| Netrc | No | Built-in |

## MSRV

The minimum supported Rust version is **1.85.0** (edition 2024).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
