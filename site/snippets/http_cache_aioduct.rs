// features: tokio,json
// runtime: tokio
use aioduct::{CacheConfig, HttpEngine, HttpCache, InMemoryCacheStore};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let cache = HttpCache::new(
        InMemoryCacheStore::new(),
        CacheConfig::default(),
    );

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
