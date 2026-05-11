// features: tokio,rustls,rustls-ring,doh
// runtime: tokio
use aioduct::HttpEngine;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .dns_over_https("1.1.1.1".parse().unwrap(), "cloudflare-dns.com")
        .build();

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Resolved via DoH: {}", resp.status());
    Ok(())
}
