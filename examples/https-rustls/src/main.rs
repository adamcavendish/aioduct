use aioduct::HttpEngine;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector).build();

    let resp = client.get("https://www.rust-lang.org/")?.send().await?;

    println!("Status: {}", resp.status());
    println!("Version: {:?}", resp.version());

    // Inspect TLS info if available
    if let Some(tls) = resp.tls_info()
        && let Some(cert) = tls.peer_certificate()
    {
        println!("Peer cert: {} bytes", cert.len());
    }

    println!("Remote addr: {:?}", resp.remote_addr());

    let body = resp.text().await?;
    println!("\nBody length: {} bytes", body.len());

    Ok(())
}
