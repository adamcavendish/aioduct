use aioduct::runtime::TokioRuntime;
use aioduct::{Client, HttpCache};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // Create an in-memory HTTP cache
    let cache = HttpCache::new();

    let client = Client::<TokioRuntime>::builder().cache(cache).build();

    // First request — fetches from server, stores in cache
    let resp = client.get("https://httpbin.org/cache/60")?.send().await?;

    println!("First request: {} (from server)", resp.status());
    let body1 = resp.text().await?;

    // Second request — served from cache (no network round-trip)
    let resp = client.get("https://httpbin.org/cache/60")?.send().await?;

    println!("Second request: {} (from cache)", resp.status());
    let body2 = resp.text().await?;

    // Cache returns the same response
    assert_eq!(body1, body2);
    println!("Cache hit confirmed — bodies match");

    // Conditional request with ETag
    let resp = client
        .get("https://httpbin.org/etag/abc123")?
        .send()
        .await?;

    println!("\nETag request: {}", resp.status());
    if let Some(etag) = resp.headers().get("etag") {
        println!("ETag: {}", etag.to_str().unwrap_or("<binary>"));
    }

    // POST invalidates the cache for that URL
    let _ = client
        .post("https://httpbin.org/cache/60")?
        .body("invalidate")
        .send()
        .await?;

    println!("\nPOST sent — cache invalidated for that URL");

    Ok(())
}
