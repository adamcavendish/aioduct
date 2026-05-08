// features: tokio,tracing,otel
// runtime: tokio
use aioduct::{Client, TracingMiddleware, OtelMiddleware};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .middleware(TracingMiddleware)
        .middleware(OtelMiddleware::default())
        .build();

    // Every request automatically emits:
    // - tracing spans with method, url, status, duration
    // - OpenTelemetry spans compatible with Jaeger/Zipkin/OTLP
    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Status: {}", resp.status());
    Ok(())
}
