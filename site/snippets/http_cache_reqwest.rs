// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest does not have built-in HTTP caching.
// You'd need to implement your own caching layer:
// - Parse Cache-Control, ETag, Last-Modified headers
// - Manage a cache store (in-memory or on-disk)
// - Handle conditional requests (If-None-Match)
// - Implement cache invalidation
//
// Or use http-cache-reqwest (third-party middleware).

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let client = reqwest::Client::new();
    // No caching — every request hits the network
    let r1 = client.get("https://httpbin.org/cache/60").send().await?;
    let r2 = client.get("https://httpbin.org/cache/60").send().await?;
    println!("{} {}", r1.status(), r2.status());
    Ok(())
}
