use aioduct::CompioClient;
use aioduct::runtime::compio_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = CompioClient::builder_local(TcpConnector).build_local();

        let resp = client
            .get_local("https://www.rust-lang.org/")?
            .send()
            .await?;

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
