# Proxy Support

aioduct supports routing requests through HTTP and SOCKS5 proxies. For HTTP targets via an HTTP proxy, the request is sent directly to the proxy. For HTTPS targets, a CONNECT tunnel is established. SOCKS5 proxies tunnel all traffic regardless of scheme.

## Basic Usage

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};
use aioduct::runtime::tokio_rt::TcpConnector;

// HTTP proxy
let client = TokioClient::builder(TcpConnector)
    .proxy(ProxyConfig::http("http://proxy.example.com:8080").unwrap())
    .build()?;

// SOCKS5 proxy
let client = TokioClient::builder(TcpConnector)
    .proxy(ProxyConfig::socks5("socks5://socks-proxy.example.com:1080").unwrap())
    .build()?;
```

## System Proxy (Environment Variables)

Use `system_proxy()` to read proxy settings from environment variables:

```rust,no_run
use aioduct::TokioClient;
use aioduct::runtime::tokio_rt::TcpConnector;

let client = TokioClient::builder(TcpConnector)
    .system_proxy()
    .build()?;
```

This reads:
- `HTTP_PROXY` / `http_proxy` — proxy for HTTP requests
- `HTTPS_PROXY` / `https_proxy` — proxy for HTTPS requests
- `NO_PROXY` / `no_proxy` — comma-separated list of hosts to bypass

The uppercase variant takes precedence over the lowercase variant.

### NO_PROXY Rules

The `NO_PROXY` value is a comma-separated list of patterns:

| Pattern | Matches |
|---------|---------|
| `example.com` | `example.com` and `*.example.com` |
| `.example.com` | `*.example.com` (subdomains only) |
| `*` | All hosts (disables proxy) |
| `127.0.0.1` | Exact IP match |

## Advanced: Separate HTTP/HTTPS Proxies

Use `ProxySettings` for fine-grained control:

```rust,no_run
use aioduct::{TokioClient, ProxyConfig, ProxySettings, NoProxy};
use aioduct::runtime::tokio_rt::TcpConnector;

let settings = ProxySettings::all(
    ProxyConfig::http("http://proxy.example.com:8080").unwrap()
)
.no_proxy(NoProxy::new("localhost, .internal.corp, 10.0.0.0/8"));

let client = TokioClient::builder(TcpConnector)
    .proxy_settings(settings)
    .build()?;
```

You can also set different proxies for HTTP and HTTPS:

```rust,no_run
# use aioduct::{TokioClient, ProxyConfig, ProxySettings, NoProxy};
# use aioduct::runtime::tokio_rt::TcpConnector;
let settings = ProxySettings::default()
    .http(ProxyConfig::http("http://http-proxy:3128").unwrap())
    .https(ProxyConfig::http("http://https-proxy:3129").unwrap())
    .no_proxy(NoProxy::new("localhost"));

let client = TokioClient::builder(TcpConnector)
    .proxy_settings(settings)
    .build()?;
```

## Proxy Authentication

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};
use aioduct::runtime::tokio_rt::TcpConnector;

let client = TokioClient::builder(TcpConnector)
    .proxy(
        ProxyConfig::http("http://proxy.example.com:8080")
            .unwrap()
            .basic_auth("user", "pass"),
    )
    .build()?;
```

## How It Works

### HTTP Targets

For plain HTTP requests, the client connects to the proxy and sends the request with the full absolute URI in the request line. The proxy forwards the request to the target server.

### HTTPS Targets (CONNECT Tunnel)

For HTTPS requests, the client:

1. Connects to the proxy via TCP
2. Sends `CONNECT host:port HTTP/1.1` to establish a tunnel
3. Waits for a `200` response from the proxy
4. Performs TLS handshake through the tunnel
5. Sends the actual HTTPS request over the encrypted connection

This ensures end-to-end encryption — the proxy only sees the target hostname, not the request content.

## Example: Corporate Proxy

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder(TcpConnector)
        .proxy(
            ProxyConfig::http("http://corporate-proxy:3128")
                .unwrap()
                .basic_auth("employee", "password"),
        )
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .build()?;

    let resp = client
        .get("https://api.example.com/data")?
        .send()
        .await?;

    println!("{}", resp.text().await?);
    Ok(())
}
```

## SOCKS4/SOCKS4a Proxy

SOCKS4/SOCKS4a proxies are also supported. SOCKS4a extends SOCKS4 with domain name resolution on the proxy side:

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};
use aioduct::runtime::tokio_rt::TcpConnector;

// SOCKS4a proxy (domain resolution on proxy)
let client = TokioClient::builder(TcpConnector)
    .proxy(ProxyConfig::socks4("socks4a://localhost:1080").unwrap())
    .build()?;

// SOCKS4 proxy
let client = TokioClient::builder(TcpConnector)
    .proxy(ProxyConfig::socks4("socks4://localhost:1080").unwrap())
    .build()?;
```

SOCKS4 supports optional user ID authentication (passed via `basic_auth` — only the username is used).

Environment variables with `socks4://` or `socks4a://` URLs are automatically detected by `system_proxy()`.

## SOCKS5 Proxy

SOCKS5 proxies tunnel TCP connections at a lower level than HTTP proxies. After the SOCKS5 handshake, the TCP stream is used directly — for HTTP targets, the client sends a normal request; for HTTPS targets, TLS is negotiated over the tunnel.

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};
use aioduct::runtime::tokio_rt::TcpConnector;

// Without auth
let client = TokioClient::builder(TcpConnector)
    .proxy(ProxyConfig::socks5("socks5://localhost:1080").unwrap())
    .build()?;

// With username/password auth
let client = TokioClient::builder(TcpConnector)
    .proxy(
        ProxyConfig::socks5("socks5://localhost:1080")
            .unwrap()
            .basic_auth("user", "pass"),
    )
    .build()?;
```

Environment variables with `socks5://` URLs are automatically detected by `system_proxy()`.

## Limitations

- SOCKS5 supports no-auth and username/password authentication (RFC 1928/1929)
- SOCKS4 supports optional user ID authentication
- The HTTP proxy URI must use `http://` scheme; the SOCKS5 proxy URI must use `socks5://`; the SOCKS4 proxy URI must use `socks4://` or `socks4a://`
