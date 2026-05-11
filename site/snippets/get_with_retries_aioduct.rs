// features: tokio,json
// runtime: tokio
use aioduct::HttpEngine;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .retry(aioduct::RetryConfig::default().max_retries(3))
        .build();

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    let data: serde_json::Value = resp.json().await?;
    println!("{data:#}");
    Ok(())
}
