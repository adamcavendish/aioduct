use aioduct::CompioClient;
use aioduct::runtime::compio_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Force HTTP/2 without TLS upgrade negotiation (h2c)
        // This is useful for local services that speak HTTP/2 directly
        let _client = CompioClient::builder_local(TcpConnector)
            .http2_prior_knowledge()
            .build_local();

        // Note: most public servers don't support h2c, so we only demonstrate the API.
        // Use the h2c client with a local server that supports cleartext HTTP/2.
        println!("Client configured for HTTP/2 prior knowledge (h2c)");
        println!("Use with a local server that supports cleartext HTTP/2");

        // For a real HTTPS HTTP/2 connection:
        // The client automatically negotiates HTTP/2 via ALPN during TLS handshake
        // when the server supports it. No special configuration needed.

        // Example with a standard HTTPS endpoint (negotiates h2 via ALPN):
        let standard_client = CompioClient::builder_local(TcpConnector).build_local();

        let resp = standard_client
            .get_local("https://httpbin.org/get")?
            .send()
            .await?;

        println!("\nHTTPS negotiated version: {:?}", resp.version());
        println!("Status: {}", resp.status());

        Ok(())
    })
}
