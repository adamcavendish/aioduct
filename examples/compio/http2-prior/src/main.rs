use aioduct::CompioClient;
use aioduct::runtime::compio_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Force HTTP/2 without TLS upgrade negotiation (h2c)
        // This is useful for local services that speak HTTP/2 directly
        let _client = CompioClient::builder_local(TcpConnector)
            .http2_prior_knowledge()
            .build_local();

        // Note: httpbin.org doesn't support h2c, so this would fail in practice.
        // This example demonstrates the API for local h2c servers.
        println!("Client configured for HTTP/2 prior knowledge (h2c)");
        println!("Use with a local server that supports cleartext HTTP/2");

        // For a real HTTPS HTTP/2 connection:
        // The client automatically negotiates HTTP/2 via ALPN during TLS handshake
        // when the server supports it. No special configuration needed.

        // Example with a standard HTTPS endpoint (negotiates h2 via ALPN):
        let standard_client = CompioClient::builder_local(TcpConnector).build_local();

        let resp = standard_client
            .get_local("https://www.google.com/")?
            .send()
            .await?;

        println!("\nHTTPS negotiated version: {:?}", resp.version());
        println!("Status: {}", resp.status());

        Ok(())
    })
}
