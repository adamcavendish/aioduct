use aioduct::SmolClient;
use aioduct::runtime::smol_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = SmolClient::builder(TcpConnector).build();

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
    })
}
