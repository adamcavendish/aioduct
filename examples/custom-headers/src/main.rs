use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // Set default headers on the client
    let mut default_headers = http::HeaderMap::new();
    default_headers.insert("x-custom-global", "from-client".parse().unwrap());

    let client = Client::<TokioRuntime>::builder()
        .user_agent("my-app/1.0")
        .default_headers(default_headers)
        .build();

    // Per-request headers
    let resp = client
        .get("https://httpbin.org/headers")?
        .header(
            http::header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        )
        .header(
            http::header::HeaderName::from_static("x-request-id"),
            http::HeaderValue::from_static("abc-123"),
        )
        .send()
        .await?;

    println!("Headers echoed back:\n{}", resp.text().await?);

    // Bearer auth
    let resp = client
        .get("https://httpbin.org/bearer")?
        .bearer_auth("my-token-here")
        .send()
        .await?;

    println!("\nBearer auth status: {}", resp.status());

    // Basic auth
    let resp = client
        .get("https://httpbin.org/basic-auth/user/pass")?
        .basic_auth("user", Some("pass"))
        .send()
        .await?;

    println!("Basic auth status: {}", resp.status());

    Ok(())
}
