// comparison: reqwest equivalent
// NOT compiled in CI (external crate)
use reqwest;

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    // reqwest has no built-in retry — you need
    // reqwest-middleware + reqwest-retry crates
    let client = reqwest::Client::new();

    let resp = client.get("https://httpbin.org/get").send().await?;
    let data: serde_json::Value = resp.json().await?;
    println!("{data:#}");
    Ok(())
}
