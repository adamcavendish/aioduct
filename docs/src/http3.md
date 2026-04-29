# HTTP/3

aioduct has experimental HTTP/3 support via [h3](https://crates.io/crates/h3), [h3-quinn](https://crates.io/crates/h3-quinn), and [quinn](https://crates.io/crates/quinn).

## Feature Flag

Enable the `http3` transport feature with the rustls backend and a rustls crypto provider:

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio", "http3", "rustls", "rustls-ring"] }
```

To use AWS-LC instead of ring, select the AWS-LC rustls provider:

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio", "http3", "rustls", "rustls-aws-lc-rs"] }
```

The `http3` feature only selects the QUIC/HTTP/3 transport dependencies. Today HTTP/3 still requires the rustls backend because quinn uses rustls for QUIC TLS; choose exactly one of `rustls-ring` or `rustls-aws-lc-rs`.

## Usage

There are two modes for HTTP/3:

### Always-H3 Mode

Force all HTTPS requests through QUIC/HTTP/3:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

// All HTTPS requests will use HTTP/3
let client = Client::<TokioRuntime>::with_http3();
```

Or via the builder:

```rust,no_run
use aioduct::Client;
use aioduct::tls::RustlsConnector;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .http3(true)
    .build();
```

### Alt-Svc Auto-Upgrade Mode

Start with HTTP/1.1 or HTTP/2 over TCP, and automatically upgrade to HTTP/3 when the server advertises it via the `Alt-Svc` header:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

// First request uses TCP; upgrades to QUIC when Alt-Svc is seen
let client = Client::<TokioRuntime>::with_alt_svc_h3();
```

Or via the builder:

```rust,no_run
use aioduct::Client;
use aioduct::tls::RustlsConnector;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .tls(RustlsConnector::with_webpki_roots())
    .alt_svc_h3(true)
    .build();
```

> **Important:** `.tls()` must be called before `.http3(true)` or `.alt_svc_h3(true)` when you provide a custom TLS connector because HTTP/3 reuses that rustls configuration to build the QUIC endpoint.

## Alt-Svc Protocol Upgrade

When Alt-Svc auto-upgrade is enabled (`.alt_svc_h3(true)` or `with_alt_svc_h3()`):

1. The first request to a new origin goes over TCP (HTTP/1.1 or HTTP/2 via ALPN).
2. If the response includes an `Alt-Svc` header advertising `h3` (e.g., `Alt-Svc: h3=":443"; ma=86400`), the client caches this.
3. Subsequent requests to the same origin use QUIC/HTTP/3 instead of TCP.
4. The cache respects `ma` (max-age) — entries expire after the specified duration (default 24 hours).
5. `Alt-Svc: clear` removes cached entries, reverting to TCP for that origin.

The Alt-Svc cache supports alternate hosts and ports. For example, `h3="alt.example.com:8443"` routes QUIC traffic to a different endpoint while keeping the original host for SNI.

## How It Works

When HTTP/3 is enabled (either mode):

1. **HTTPS requests** are sent over QUIC using the quinn transport. The client opens a QUIC connection, performs the TLS 1.3 handshake, and sends the request via the h3 protocol.
2. **HTTP requests** (plain) continue to use TCP-based HTTP/1.1 or HTTP/2 as usual.
3. **Connection pooling** works for QUIC connections the same way it does for TCP — the pool checks for an existing idle connection to the same `(scheme, authority)` before establishing a new one. Like HTTP/2, HTTP/3 multiplexes streams over a single connection.

When HTTP/3 is **not** enabled (default), the client uses TCP with HTTP/1.1 or HTTP/2 negotiated via ALPN, even for HTTPS.

## Limitations

- **Experimental** — the h3 ecosystem (h3 0.0.8, h3-quinn 0.0.10) is pre-1.0.
- **No fallback** — in always-h3 mode, if the server doesn't support QUIC, the request fails rather than falling back to TCP. Use Alt-Svc mode or the default (non-h3) client for servers where QUIC support is uncertain.
- **Tokio only** — quinn requires tokio, so HTTP/3 is only available with the `tokio` runtime feature.
- **rustls required today** — future TLS backend work may change the available combinations, but current HTTP/3 support composes with rustls provider features.
