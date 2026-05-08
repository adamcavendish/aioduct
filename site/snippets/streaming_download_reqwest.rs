// comparison: reqwest equivalent
// NOT compiled in CI (external crate)
use reqwest;
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // reqwest has no built-in bandwidth limiting
    // you'd need to manually throttle the stream
    let client = reqwest::Client::new();

    let resp = client
        .get("https://httpbin.org/drip?numbytes=5000000&duration=5")
        .send()
        .await?;

    let mut stream = resp.bytes_stream();
    let mut total = 0;
    while let Some(chunk) = stream.next().await {
        total += chunk?.len();
    }
    println!("Downloaded {total} bytes (no bandwidth limiting)");
    Ok(())
}
