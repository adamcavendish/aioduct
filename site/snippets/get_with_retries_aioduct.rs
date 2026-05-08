// features: tokio,json
// runtime: tokio
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .retry(aioduct::RetryConfig::default().max_retries(3))
        .build();

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    let data: serde_json::Value = resp.json().await?;
    println!("{data:#}");
    Ok(())
}
