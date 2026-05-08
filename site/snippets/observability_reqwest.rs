// comparison: reqwest equivalent
// NOT compiled in CI (external crate)

// reqwest has no built-in observability.
// For tracing: wrap every request manually, or use
// reqwest-tracing (third-party middleware crate).
// For OpenTelemetry: no official integration exists.
// You'd need to manually create spans around each request.

use reqwest;

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let client = reqwest::Client::new();
    // Manual instrumentation required:
    // let span = tracing::info_span!("http_request", ...);
    // let _guard = span.enter();
    let resp = client.get("https://httpbin.org/get").send().await?;
    println!("{}", resp.status());
    Ok(())
}
