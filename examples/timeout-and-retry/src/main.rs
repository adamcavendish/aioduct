use std::time::Duration;

use aioduct::runtime::TokioRuntime;
use aioduct::{Client, RetryConfig};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        // Connection timeout: max time to establish TCP + TLS
        .connect_timeout(Duration::from_secs(5))
        // Request timeout: max total time per request attempt
        .timeout(Duration::from_secs(10))
        // Read timeout: max time between body chunks
        .read_timeout(Duration::from_secs(5))
        // Retry on 5xx errors and network failures
        .retry(
            RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(100))
                .max_backoff(Duration::from_secs(2)),
        )
        .build();

    // This will retry up to 3 times on failure
    let resp = client.get("https://httpbin.org/get")?.send().await?;

    println!("Status: {}", resp.status());
    println!("Body:\n{}", resp.text().await?);

    // Per-request timeout override
    let result = client
        .get("https://httpbin.org/delay/10")?
        .timeout(Duration::from_secs(2))
        .send()
        .await;

    match result {
        Err(aioduct::Error::Timeout) => println!("\nRequest timed out as expected!"),
        Ok(resp) => println!("\nGot response: {}", resp.status()),
        Err(e) => println!("\nOther error: {e}"),
    }

    Ok(())
}
