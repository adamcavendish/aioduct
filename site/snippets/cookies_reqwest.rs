// comparison: reqwest equivalent
// NOT compiled in CI (external crate)
use reqwest;

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()?;

    // Login: server sets session cookie
    client.post("https://httpbin.org/cookies/set/session/abc123")
        .send().await?;

    // Subsequent request: cookie sent automatically
    let resp = client.get("https://httpbin.org/cookies").send().await?;
    let cookies: serde_json::Value = resp.json().await?;
    println!("Cookies: {cookies:#}");
    Ok(())
}
