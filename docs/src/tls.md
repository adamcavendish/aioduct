# TLS & HTTPS

aioduct supports HTTPS via [rustls](https://github.com/rustls/rustls). No TLS library is included by default — plain HTTP works without any TLS dependency.

## Enabling HTTPS

Use the `rustls` TLS backend with the ring crypto provider:

```toml
[dependencies]
aioduct = { version = "0.2.1", features = ["tokio", "rustls", "rustls-ring"] }
```

Use the same rustls backend with the AWS-LC crypto provider:

```toml
[dependencies]
aioduct = { version = "0.2.1", features = ["tokio", "rustls", "rustls-aws-lc-rs"] }
```

Add `rustls-native-roots` alongside either provider to use the OS certificate store:

```toml
[dependencies]
aioduct = { version = "0.2.1", features = ["tokio", "rustls-native-roots", "rustls-aws-lc-rs"] }
```

## Quick Start

```rust,no_run
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // with_rustls() configures WebPKI root certificates automatically
    let client = TokioClient::with_rustls();

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

`TokioClient::with_rustls()` uses [webpki-roots](https://crates.io/crates/webpki-roots), which bundles Mozilla's root certificate store directly in the binary. No system certificate store access is needed.

Enable `rustls-native-roots` to build the connector from the operating system certificate store instead. This feature enables the rustls backend but does not select a crypto provider by itself; combine it with either `rustls-ring` or `rustls-aws-lc-rs`.

### Crypto Providers

The `rustls` feature enables the rustls TLS backend, while `rustls-ring` and `rustls-aws-lc-rs` select the crypto provider. Enable exactly one provider whenever `rustls` is enabled; enabling neither or both is a compile error.

The backend/provider split keeps room for future TLS backends. A `native-tls` backend name is reserved for possible OpenSSL/native TLS support, but it is not implemented today.

## Runtime Scope

| Runtime | TLS provider | Configuration surface |
|---------|--------------|-----------------------|
| Tokio | rustls | Builder TLS methods and custom `RustlsConnector` |
| smol | rustls | Same TLS configuration as Tokio |
| compio | rustls | Same TLS configuration through the local client path |
| blocking | Wrapped native client | Inherits the configured async client TLS behavior |
| wasm | Browser-managed | Certificate verification, SNI, ALPN, and roots are controlled by the browser |
| wasi-p2 | Host-managed | Certificate verification, SNI, ALPN, and roots are controlled by the WASI host |

Native clients expose TLS version bounds, SNI enablement, extra root certificates,
client identity, CRLs, hostname-verification bypass for tests, and a fully custom
rustls configuration. Browser and WASI clients intentionally do not duplicate
host-managed TLS policy knobs.

For operator-provided CA bundles, use the fallible PEM bundle path:

```rust,no_run
use aioduct::TokioClient;

# fn build_client() -> Result<TokioClient, Box<dyn std::error::Error>> {
let client = TokioClient::builder()
    .add_root_certificates_pem_bundle(include_bytes!("ca.pem"))?
    .build()?;
# Ok(client)
# }
```

This parser rejects empty bundles, private keys, unsupported PEM sections,
malformed input, and certificates rustls will not accept as trust roots. It is
the preferred path for host policy code that reads operator configuration.

## Custom TLS Configuration

For advanced use cases, configure the `RustlsConnector` directly:

```rust,no_run
use aioduct::TokioClient;
use aioduct::tls::RustlsConnector;

let client = TokioClient::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .build()?;
```

### Encrypted ClientHello

Encrypted ClientHello (ECH) is available through rustls custom client
configuration. Because ECH configuration is domain-specific and rustls forces
TLS 1.3 when ECH is enabled, build the `rustls::ClientConfig` yourself and pass
it to `RustlsConnector::new`.

This example uses rustls and webpki-roots APIs directly, so applications should
declare those crates explicitly when building custom ECH configurations.

```rust,no_run
use std::sync::Arc;

use aioduct::TokioClient;
use aioduct::tls::RustlsConnector;
use rustls::client::{EchConfig, EchMode};
use rustls::pki_types::EchConfigListBytes;

# fn build_client(ech_config_list_bytes: Vec<u8>) -> Result<TokioClient, Box<dyn std::error::Error>> {
let hpke_suites = rustls::crypto::aws_lc_rs::hpke::ALL_SUPPORTED_SUITES;
let ech_config = EchConfig::new(
    EchConfigListBytes::from(ech_config_list_bytes),
    hpke_suites,
)?;
let root_store =
    rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
let mut config = rustls::ClientConfig::builder_with_provider(provider)
    .with_ech(EchMode::Enable(ech_config))?
    .with_root_certificates(root_store)
    .with_no_client_auth();

config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

let client = TokioClient::builder()
    .tls(RustlsConnector::new(Arc::new(config)))
    .build()?;
# Ok(client)
# }
```

The `ech_config_list_bytes` value comes from the `ech` parameter of the
server's DNS HTTPS record after base64 decoding. Use this path for Tokio, smol,
compio, and blocking clients; they all share the same rustls connector.

## Accepting Invalid Certificates

For development and testing, you can disable certificate verification:

```rust,no_run
use aioduct::TokioClient;

let client = TokioClient::builder()
    .danger_accept_invalid_certs()
    .build()?;
```

> **Warning**: Never use this in production. It disables all certificate verification, making the connection vulnerable to MITM attacks.

## HTTPS-Only Mode

To enforce that all requests use HTTPS:

```rust,no_run
use aioduct::TokioClient;
use aioduct::tls::RustlsConnector;

let client = TokioClient::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .https_only(true)
    .build()?;

// This will return an error:
// client.get("http://example.com")?.send().await?;
```

## Error Handling

TLS errors surface as `Error::Tls(Box<dyn std::error::Error + Send + Sync>)`. Common failure modes:

- Certificate verification failure (expired, wrong hostname, untrusted CA)
- No TLS connector configured (HTTPS URL without the `rustls` backend and a rustls provider, or without a `.tls()` builder call for a custom connector)
- Handshake timeout (use `.timeout()` on the request or client)
