use aioduct::runtime::TokioRuntime;
use aioduct::{Client, TracingMiddleware};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // Initialize tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter("aioduct=trace,example_tracing_logs=debug")
        .init();

    tracing::info!("starting tracing example");

    let client = Client::<TokioRuntime>::builder()
        .middleware(TracingMiddleware)
        .build();

    // Each request will emit tracing spans and events
    let resp = client.get("https://httpbin.org/get")?.send().await?;

    tracing::info!(status = %resp.status(), "received response");

    let _body = resp.text().await?;

    // Request that triggers a redirect — visible in traces
    let resp = client.get("https://httpbin.org/redirect/1")?.send().await?;

    tracing::info!(final_url = %resp.url(), "redirect completed");

    // Request that will fail — error event emitted
    let result = client.get("https://httpbin.org/status/500")?.send().await;

    match result {
        Ok(resp) => tracing::warn!(status = %resp.status(), "got error status"),
        Err(e) => tracing::error!(error = %e, "request failed"),
    }

    Ok(())
}
