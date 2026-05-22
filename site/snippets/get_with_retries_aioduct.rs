// features: tokio,json
// runtime: tokio
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder()
        .retry(aioduct::RetryConfig::default().max_retries(3))
        .build()?;

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    let data: serde_json::Value = resp.json().await?;
    println!("{data:#}");
    Ok(())
}
