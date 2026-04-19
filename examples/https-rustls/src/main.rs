use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

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
