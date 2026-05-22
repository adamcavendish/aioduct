// features: tokio,rustls,rustls-ring,http3
// runtime: tokio
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .http3(true)?
        .build()?;

    // Automatically upgrades to HTTP/3 via Alt-Svc
    let resp = client.get("https://cloudflare.com/")?.send().await?;
    println!("Version: {:?}", resp.version());
    println!("Status: {}", resp.status());
    Ok(())
}
