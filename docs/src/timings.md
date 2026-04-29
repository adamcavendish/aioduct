# Request Timings

aioduct records the duration of each connection phase for every request. Access the breakdown via `Response::timings()`.

## Usage

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::with_rustls();

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

## Pool Hits

When a request reuses an existing pooled connection, the connection setup phases (DNS, TCP, TLS) are skipped and reported as `None`. Only `transfer()` and `total()` are populated.

## HTTP/3

HTTP/3 connections include QUIC connection establishment time. The TLS handshake is part of the QUIC handshake and is reflected in the `tls_handshake()` timing.
