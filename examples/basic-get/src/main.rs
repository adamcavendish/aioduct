use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    // Simple GET request
    let resp = client.get("https://httpbin.org/get")?.send().await?;

    println!("Status: {}", resp.status());
    println!("URL: {}", resp.url());
    println!("Version: {:?}", resp.version());

    // Read response headers
    for (name, value) in resp.headers() {
        println!("  {name}: {}", value.to_str().unwrap_or("<binary>"));
    }

    // Read body as text
    let body = resp.text().await?;
    println!("\nBody:\n{body}");

    Ok(())
}
