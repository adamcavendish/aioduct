use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use aioduct::{RetryConfig, RetryDecision, SmolClient};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};

fn observed_retries(attempts: Arc<AtomicU32>) -> RetryConfig {
    RetryConfig::default()
        .max_retries(3)
        .initial_backoff(Duration::from_millis(100))
        .max_backoff(Duration::from_secs(2))
        .classify(move |context| {
            attempts.store(context.attempt() + 1, Ordering::Relaxed);
            RetryDecision::UseDefault
        })
}

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = SmolClient::builder()
            // Connection timeout: max time to establish TCP + TLS
            .connect_timeout(Duration::from_secs(5))
            // Request timeout: max total time per request attempt
            .timeout(Duration::from_secs(10))
            // Read timeout: max time between body chunks
            .read_timeout(Duration::from_secs(5))
            // Write timeout: max time between upload chunks
            .write_timeout(Duration::from_secs(5))
            // Retry on 5xx errors and network failures
            .retry(
                RetryConfig::default()
                    .max_retries(3)
                    .initial_backoff(Duration::from_millis(100))
                    .max_backoff(Duration::from_secs(2)),
            )
            .build()
            .unwrap();

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
            Err(e) if e.is_timeout() => println!("\nRequest timed out as expected!"),
            Ok(resp) => println!("\nGot response: {}", resp.status()),
            Err(e) => println!("\nOther error: {e}"),
        }

        // Buffered bodies can be reproduced byte-for-byte for configured retries.
        let buffered_attempts = Arc::new(AtomicU32::new(0));
        let buffered = client
            .put("https://httpbin.org/status/503")?
            .body("buffered upload")
            .retry(observed_retries(Arc::clone(&buffered_attempts)))
            .send()
            .await?;
        println!(
            "\nBuffered body: {} attempt(s), final status {}",
            buffered_attempts.load(Ordering::Relaxed),
            buffered.status()
        );

        // Streaming bodies are one-shot. A retryable response is returned without
        // replaying the request with an empty or partially consumed body.
        let one_shot_attempts = Arc::new(AtomicU32::new(0));
        let body = Full::new(Bytes::from_static(b"one-shot upload"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let one_shot = client
            .put("https://httpbin.org/status/503")?
            .body_stream(body)
            .retry(observed_retries(Arc::clone(&one_shot_attempts)))
            .send()
            .await?;
        println!(
            "One-shot body: {} attempt(s), final status {}",
            one_shot_attempts.load(Ordering::Relaxed),
            one_shot.status()
        );

        Ok(())
    })
}
