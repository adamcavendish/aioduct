// features: tokio
// runtime: tokio
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let proxy = aioduct::ProxyConfig::http("http://proxy.corp:8080")?;

    let client = TokioClient::builder()
        .proxy(proxy)
        .build()?;

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Via proxy: {}", resp.status());
    Ok(())
}
