use std::time::Duration;

use aioduct::runtime::TokioRuntime;
use aioduct::{Client, RateLimiter};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // Rate limit to 5 requests per second
    let client = Client::<TokioRuntime>::builder()
        .rate_limiter(RateLimiter::new(5, Duration::from_secs(1)))
        .build();

    println!("Sending 10 requests with rate limit of 5/sec...");

    let start = std::time::Instant::now();

    for i in 1..=10 {
        let resp = client.get("https://httpbin.org/get")?.send().await?;

        println!(
            "[{:.1}s] Request {i}: {}",
            start.elapsed().as_secs_f64(),
            resp.status()
        );
    }

    let elapsed = start.elapsed();
    println!(
        "\n10 requests completed in {:.1}s (expected ~2s with 5/sec limit)",
        elapsed.as_secs_f64()
    );

    Ok(())
}
