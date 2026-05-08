// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest does not expose per-request timing breakdowns.
// You'd need to manually measure with Instant::now() around
// each request, but you cannot get DNS/TCP/TLS/TTFB split.
//
// For detailed timings you'd need a lower-level library
// or a tracing subscriber that captures hyper's internal spans.

use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let client = reqwest::Client::new();
    let start = Instant::now();
    let resp = client.get("https://httpbin.org/get").send().await?;
    let elapsed = start.elapsed();
    // Only total time available — no DNS/TCP/TLS split
    println!("Total: {elapsed:?}, Status: {}", resp.status());
    Ok(())
}
