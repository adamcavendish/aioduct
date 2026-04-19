# HTTP/3

aioduct has experimental HTTP/3 support via [h3](https://crates.io/crates/h3), [h3-quinn](https://crates.io/crates/h3-quinn), and [quinn](https://crates.io/crates/quinn).

## Feature Flag

Enable the `http3` feature in your `Cargo.toml`:

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio", "http3"] }
```

The `http3` feature automatically enables `rustls` since QUIC requires TLS 1.3.

## Usage

### Quick Start

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::<TokioRuntime>::with_http3();

    let resp = client
        .get("https://httpbin.org/get")?
        .send()
        .await?;

    println!("status: {}", resp.status());
    println!("{}", resp.text().await?);
    Ok(())
}
```

### Builder API

For more control, use the builder to configure TLS before enabling HTTP/3:

```rust,no_run
use aioduct::Client;
use aioduct::tls::RustlsConnector;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .http3(true)
    .build();
```

> **Important:** `.tls()` must be called before `.http3(true)` because HTTP/3
> reuses the rustls configuration to build the QUIC endpoint.

## How It Works

When HTTP/3 is enabled:

1. **HTTPS requests** are sent over QUIC using the quinn transport. The client opens a new QUIC connection, performs the TLS 1.3 handshake, and sends the request via the h3 protocol.
2. **HTTP requests** (plain) continue to use TCP-based HTTP/1.1 or HTTP/2 as usual.
3. Each HTTPS request creates a new QUIC connection. Connection pooling for QUIC is not yet implemented.

When HTTP/3 is **not** enabled (default), the client uses TCP with HTTP/1.1 or HTTP/2 negotiated via ALPN, even for HTTPS.

## Limitations

- **Experimental** — the h3 ecosystem (h3 0.0.8, h3-quinn 0.0.10) is pre-1.0.
- **No QUIC connection pooling** — each request opens a new QUIC connection. TCP connection pooling still works for non-h3 requests.
- **No Alt-Svc upgrade** — the client does not automatically discover HTTP/3 support via `Alt-Svc` headers. You must explicitly opt in.
- **No fallback** — if the server doesn't support QUIC, the request fails rather than falling back to TCP. Use the default (non-h3) client for servers where QUIC support is uncertain.
- **Tokio only** — quinn requires tokio, so HTTP/3 is only available with the `tokio` runtime feature.
