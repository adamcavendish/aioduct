# Request Timings

aioduct records the duration of each connection phase for every request. Access the breakdown via `Response::timings()`.

## Usage

```rust,no_run
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::with_rustls();

    let resp = client
        .get("https://httpbin.org/get")?
        .send()
        .await?;

    if let Some(t) = resp.timings() {
        if let Some(dns) = t.dns() {
            println!("DNS:       {:?}", dns);
        }
        if let Some(tcp) = t.tcp_connect() {
            println!("TCP:       {:?}", tcp);
        }
        if let Some(tls) = t.tls_handshake() {
            println!("TLS:       {:?}", tls);
        }
        if let Some(ttfb) = t.transfer() {
            println!("Transfer:  {:?}", ttfb);
        }
        println!("Total:     {:?}", t.total());
    }

    Ok(())
}
```

## Phases

| Accessor | Measures | `None` when |
|----------|----------|-------------|
| `dns()` | Hostname resolution | Literal IP, pool hit, or proxy-handled DNS |
| `tcp_connect()` | TCP connection establishment | Pool hit |
| `tls_handshake()` | TLS handshake (rustls) | Plain HTTP or pool hit |
| `transfer()` | Request sent → first response byte (TTFB) | Should not normally be `None` |
| `total()` | Wall-clock start to response headers | Always present |

## Timeout Phases

Timings report what happened; timeout settings decide where a request may be
cut off:

| Setting | Applies to | Error |
|---------|------------|-------|
| `connect_timeout()` | Establishing the connection path, including native proxy handshakes and TLS where applicable | `ConnectTimeout` |
| `timeout()` | Overall request deadline through response headers and body consumption | `Timeout` |
| `read_timeout()` | Gaps between response body chunks after headers arrive | `ReadTimeout` |

DNS timing is recorded for native resolution when a hostname must be resolved by
aioduct. It is `None` for literal IPs, pooled connections, SOCKS remote-DNS
paths, and platform-managed transports such as browser wasm and wasi-p2.

## Pool Hits

When a request reuses an existing pooled connection, the connection setup phases (DNS, TCP, TLS) are skipped and reported as `None`. Only `transfer()` and `total()` are populated.

## HTTP/3

HTTP/3 connections include QUIC connection establishment time. The TLS handshake is part of the QUIC handshake and is reflected in the `tls_handshake()` timing.
