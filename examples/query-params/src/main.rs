use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use serde::Serialize;

#[derive(Serialize)]
struct SearchParams {
    q: String,
    page: u32,
    per_page: u32,
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    // Simple query parameters via tuples
    let resp = client
        .get("https://httpbin.org/get")?
        .query(&[("key1", "value1"), ("key2", "value with spaces")])
        .send()
        .await?;

    println!("URL with query: {}", resp.url());
    println!("Body:\n{}", resp.text().await?);

    // Typed query parameters via serde
    let params = SearchParams {
        q: "rust http client".into(),
        page: 1,
        per_page: 20,
    };

    let resp = client
        .get("https://httpbin.org/get")?
        .query_serde(&params)?
        .send()
        .await?;

    println!("\nSerde query URL: {}", resp.url());

    // Combine both — tuple params + serde params
    let resp = client
        .get("https://httpbin.org/get")?
        .query(&[("extra", "param")])
        .query_serde(&params)?
        .send()
        .await?;

    println!("\nCombined query URL: {}", resp.url());

    Ok(())
}
