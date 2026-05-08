// features: tokio,json
// runtime: tokio
use aioduct::{Client, CookieJar};
use aioduct::runtime::TokioRuntime;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let jar = Arc::new(CookieJar::new());
    let client = Client::<TokioRuntime>::builder()
        .cookie_jar(jar.clone())
        .build();

    // Login: server sets session cookie
    client.post("https://httpbin.org/cookies/set/session/abc123")?
        .send().await?;

    // Subsequent request: cookie sent automatically
    let resp = client.get("https://httpbin.org/cookies")?.send().await?;
    let cookies: serde_json::Value = resp.json().await?;
    println!("Cookies: {cookies:#}");
    Ok(())
}
