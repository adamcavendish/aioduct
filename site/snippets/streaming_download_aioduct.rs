// features: tokio
// runtime: tokio
use aioduct::HttpEngine;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .bandwidth_limit(1_048_576) // 1 MB/s
        .build();

    let resp = client
        .get("https://httpbin.org/drip?numbytes=5000000&duration=5")?
        .send()
        .await?;

    let bytes = resp.bytes().await?;
    println!("Downloaded {} bytes with bandwidth limiting", bytes.len());
    Ok(())
}
