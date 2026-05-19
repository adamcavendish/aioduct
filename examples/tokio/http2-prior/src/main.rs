use aioduct::TokioClient;
#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // Force HTTP/2 without TLS upgrade negotiation (h2c)
    // This is useful for local services that speak HTTP/2 directly
    let _client = TokioClient::builder()
        .http2_prior_knowledge()
        .build()
        .unwrap();

    // Note: most public servers don't support h2c, so we only demonstrate the API.
    // Use the h2c client with a local server that supports cleartext HTTP/2.
    println!("Client configured for HTTP/2 prior knowledge (h2c)");
    println!("Use with a local server that supports cleartext HTTP/2");

    // For a real HTTPS HTTP/2 connection:
    // The client automatically negotiates HTTP/2 via ALPN during TLS handshake
    // when the server supports it. No special configuration needed.

    // Example with a standard HTTPS endpoint (negotiates h2 via ALPN):
    let standard_client = TokioClient::builder().build().unwrap();

    let resp = standard_client
        .get("https://httpbin.org/get")?
        .send()
        .await?;

    println!("\nHTTPS negotiated version: {:?}", resp.version());
    println!("Status: {}", resp.status());

    Ok(())
}
