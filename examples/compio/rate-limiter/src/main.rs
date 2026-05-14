use std::time::Duration;

use aioduct::runtime::compio_rt::TcpConnector;
use aioduct::{CompioClient, RateLimiter};

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Rate limit to 5 requests per second
        let client = CompioClient::builder_local(TcpConnector)
            .rate_limiter(RateLimiter::new(5, Duration::from_secs(1)))
            .timeout(Duration::from_secs(10))
            .build_local();

        println!("Sending 5 requests with rate limit of 5/sec...");

        let start = std::time::Instant::now();

        for i in 1..=5 {
            let resp = client.get_local("https://httpbin.org/get")?.send().await?;

            println!(
                "[{:.1}s] Request {i}: {}",
                start.elapsed().as_secs_f64(),
                resp.status()
            );
        }

        let elapsed = start.elapsed();
        println!(
            "\n5 requests completed in {:.1}s (expected ~1s with 5/sec limit)",
            elapsed.as_secs_f64()
        );

        Ok(())
    })
}
