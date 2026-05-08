// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest does not support connection coalescing.
// Each unique host gets its own connection, even if
// the TLS certificate's SANs cover multiple domains
// pointing to the same server.
//
// This means extra TCP + TLS handshakes for subdomains
// on shared infrastructure (CDNs, load balancers).

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let client = reqwest::Client::new();
    // These always open separate connections
    let r1 = client.get("https://example.com/").send().await?;
    let r2 = client.get("https://www.example.com/").send().await?;
    println!("{} {}", r1.status(), r2.status());
    Ok(())
}
