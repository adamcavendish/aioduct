# TLS & HTTPS

aioduct supports HTTPS via [rustls](https://github.com/rustls/rustls). No TLS library is included by default — plain HTTP works without any TLS dependency.

## Enabling HTTPS

Use the `rustls` TLS backend with the ring crypto provider:

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring"] }
```

Use the same rustls backend with the AWS-LC crypto provider:

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-aws-lc-rs"] }
```

Add `rustls-native-roots` alongside either provider to use the OS certificate store:

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio", "rustls-native-roots", "rustls-aws-lc-rs"] }
```

## Quick Start

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // with_rustls() configures WebPKI root certificates automatically
    let client = Client::<TokioRuntime>::with_rustls();

    let resp = client
        .get("https://httpbin.org/get")?
        .send()
        .await?;

    println!("status: {}", resp.status());
    Ok(())
}
```

## How It Works

### Handshake

The TLS handshake is fully async, implemented as a manual state machine:

1. `RustlsConnector` wraps a `rustls::ClientConfig` (with ALPN protocols `h2` and `http/1.1`)
2. On connect, a `TlsStream<S>` is created with the underlying TCP stream and a `rustls::ClientConnection`
3. The handshake drives `read_tls`/`write_tls` helper functions that wrap the async stream as synchronous `std::io::Read`/`Write`, using `WouldBlock` for flow control
4. Once complete, the negotiated ALPN protocol determines whether to use HTTP/1.1 or HTTP/2

### ALPN Negotiation

After the TLS handshake, the negotiated protocol is inspected:

- **`h2`** → uses `hyper::client::conn::http2::handshake`
- **`http/1.1`** (or no ALPN) → uses `hyper::client::conn::http1::handshake`

This happens transparently — the client automatically selects the best protocol for each connection.

### Root Certificates

`Client::with_rustls()` uses [webpki-roots](https://crates.io/crates/webpki-roots), which bundles Mozilla's root certificate store directly in the binary. No system certificate store access is needed.

Enable `rustls-native-roots` to build the connector from the operating system certificate store instead. This feature enables the rustls backend but does not select a crypto provider by itself; combine it with either `rustls-ring` or `rustls-aws-lc-rs`.

### Crypto Providers

The `rustls` feature enables the rustls TLS backend, while `rustls-ring` and `rustls-aws-lc-rs` select the crypto provider. Enable exactly one provider whenever `rustls` is enabled; enabling neither or both is a compile error.

The backend/provider split keeps room for future TLS backends. A `native-tls` backend name is reserved for possible OpenSSL/native TLS support, but it is not implemented today.

## Custom TLS Configuration

For advanced use cases, configure the `RustlsConnector` directly:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use aioduct::tls::RustlsConnector;

let connector = RustlsConnector::with_webpki_roots();
let client = Client::<TokioRuntime>::builder()
    .tls(connector)
    .build();
```

## Accepting Invalid Certificates

For development and testing, you can disable certificate verification:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .danger_accept_invalid_certs()
    .build();
```

> **Warning**: Never use this in production. It disables all certificate verification, making the connection vulnerable to MITM attacks.

## HTTPS-Only Mode

To enforce that all requests use HTTPS:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use aioduct::tls::RustlsConnector;

let client = Client::<TokioRuntime>::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .https_only(true)
    .build();

// This will return an error:
// client.get("http://example.com")?.send().await?;
```

## Error Handling

TLS errors surface as `Error::Tls(Box<dyn std::error::Error + Send + Sync>)`. Common failure modes:

- Certificate verification failure (expired, wrong hostname, untrusted CA)
- No TLS connector configured (HTTPS URL without the `rustls` backend and a rustls provider, or without a `.tls()` builder call for a custom connector)
- Handshake timeout (use `.timeout()` on the request or client)
