// features: tokio
// runtime: tokio
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
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
