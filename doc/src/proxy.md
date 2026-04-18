# Proxy Support

aioduct supports routing requests through an HTTP proxy. For HTTP targets, the request is sent directly to the proxy. For HTTPS targets, a CONNECT tunnel is established through the proxy before TLS negotiation.

## Basic Usage

```rust,no_run
use aioduct::{Client, ProxyConfig};
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .proxy(ProxyConfig::http("http://proxy.example.com:8080").unwrap())
    .build();
```

## Proxy Authentication

```rust,no_run
use aioduct::{Client, ProxyConfig};
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .proxy(
        ProxyConfig::http("http://proxy.example.com:8080")
            .unwrap()
            .basic_auth("user", "pass"),
    )
    .build();
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
use aioduct::{Client, ProxyConfig};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .proxy(
            ProxyConfig::http("http://corporate-proxy:3128")
                .unwrap()
                .basic_auth("employee", "password"),
        )
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .build();

    let resp = client
        .get("https://api.example.com/data")?
        .send()
        .await?;

    println!("{}", resp.text().await?);
    Ok(())
}
```

## Limitations

- Only HTTP proxy protocol is supported (not SOCKS5)
- The proxy URI must use `http://` scheme
- All requests on the client go through the configured proxy (no per-request proxy or bypass rules yet)
