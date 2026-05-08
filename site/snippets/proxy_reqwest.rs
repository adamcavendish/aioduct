// comparison: reqwest equivalent
// NOT compiled in CI (external crate)
use reqwest;

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let proxy = reqwest::Proxy::all("http://proxy.corp:8080")?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .build()?;

    let resp = client.get("https://httpbin.org/get").send().await?;
    println!("Via proxy: {}", resp.status());
    Ok(())
}
