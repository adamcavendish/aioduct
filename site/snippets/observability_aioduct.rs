// features: tokio,tracing,otel
// runtime: tokio
use aioduct::{HttpEngine, TracingMiddleware, OtelMiddleware};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
