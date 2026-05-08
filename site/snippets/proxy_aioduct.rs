// features: tokio
// runtime: tokio
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .proxy(aioduct::ProxyConfig::http("http://proxy.corp:8080"))
        .proxy(aioduct::ProxyConfig::https("http://proxy.corp:8080"))
        .build();

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Via proxy: {}", resp.status());
    Ok(())
}
