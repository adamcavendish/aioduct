# aioduct

[![CI](https://github.com/adamcavendish/aioduct/actions/workflows/ci.yml/badge.svg)](https://github.com/adamcavendish/aioduct/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

Async-native Rust HTTP client built directly on **hyper 1.x** — no hyper-util, no legacy APIs.

[Documentation](https://adamcavendish.github.io/aioduct/) | [API Reference](https://docs.rs/aioduct) | [Crates.io](https://crates.io/crates/aioduct)

## Why aioduct?

- **reqwest** depends on hyper-util's `legacy::Client`, wrapping hyper 0.x-style patterns over hyper 1.x with years of backwards-compatibility baggage.
- **hyper-util** labels its own client as "legacy" — the hyper team acknowledges it's not the long-term answer.
- **hyper 1.x** provides clean connection-level primitives, but no production client uses them directly.

aioduct uses hyper 1.x **the way it was intended** — as a protocol engine you drive yourself, with your own connection pool, TLS, and runtime integration.

## Features

- **No hyper-util** — custom IO adapters and executor directly against `hyper::rt` traits
- **Multi-runtime** — tokio, smol, and compio (io_uring) via feature flags
- **rustls TLS** — async handshake with ALPN-based HTTP/1.1 and HTTP/2 negotiation
- **Connection pooling** — keyed by (scheme, authority) with idle timeout and per-host limits
- **Redirect following** — RFC-compliant handling of 301/302/303/307/308 with sensitive header stripping
- **Timeouts** — per-request, client-level, and connect timeouts
- **Decompression** — automatic gzip, brotli, zstd, deflate response decompression
- **Proxy** — HTTP proxy with CONNECT tunneling, system proxy detection (HTTP_PROXY/NO_PROXY)
- **Custom DNS** — pluggable resolver via the `Resolve` trait
- **JSON** — optional `json` feature for request/response serialization
- **Auth helpers** — bearer token, basic auth
- **Form data** — URL-encoded form bodies
- **Query parameters** — with percent-encoding
- **Default headers** — automatic User-Agent, configurable defaults

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
| `rustls`  | TLS via rustls (required for HTTPS)    | Stable       |
| `json`    | JSON request/response with serde       | Stable       |
| `gzip`    | Gzip response decompression            | Stable       |
| `deflate` | Deflate response decompression         | Stable       |
| `brotli`  | Brotli response decompression          | Stable       |
| `zstd`    | Zstd response decompression            | Stable       |
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

## Architecture

```
Client<R: Runtime>
  ├── RequestBuilder      ← fluent API (headers, body, auth, query, timeout)
  ├── ConnectionPool<R>   ← keyed by (scheme, authority), idle eviction
  ├── TLS (rustls)        ← async handshake, ALPN → h1/h2
  └── Runtime trait       ← TcpStream, Sleep, spawn, resolve
       ├── TokioRuntime
       ├── SmolRuntime
       └── CompioRuntime (placeholder)
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
| Runtime | tokio only | tokio / smol / compio |
| TLS | rustls or native-tls | rustls |
| HTTP/3 | Experimental | Experimental |
| io_uring | No | Via compio |
| Connection pool | hyper-util legacy | Custom h1/h2/h3 |

## MSRV

The minimum supported Rust version is **1.85.0** (edition 2024).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
