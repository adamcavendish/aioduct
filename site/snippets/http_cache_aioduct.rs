// features: tokio,json
// runtime: tokio
use aioduct::{CacheConfig, Client, HttpCache, InMemoryCacheStore};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let cache = HttpCache::new(
        InMemoryCacheStore::new(),
        CacheConfig::default(),
    );

    let client = Client::<TokioRuntime>::builder()
        .cache(cache)
        .build();

    // First request: fetches from server, caches response
    let r1 = client.get("https://httpbin.org/cache/60")?.send().await?;
    println!("Request 1: {} (from network)", r1.status());

    // Second request: served from cache (no network)
    let r2 = client.get("https://httpbin.org/cache/60")?.send().await?;
    println!("Request 2: {} (from cache)", r2.status());
    Ok(())
}
