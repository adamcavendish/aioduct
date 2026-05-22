// features: tokio,rustls,rustls-ring
// runtime: tokio
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .connection_coalescing(true)
        .build()?;

    // Both requests may share a single TLS connection
    // if the cert covers both domains and DNS resolves to the same IP
    let r1 = client.get("https://example.com/")?.send().await?;
    let r2 = client.get("https://www.example.com/")?.send().await?;
    println!("{} {}", r1.status(), r2.status());
    Ok(())
}
