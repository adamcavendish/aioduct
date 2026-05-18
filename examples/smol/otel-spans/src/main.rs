use std::time::Duration;

use aioduct::runtime::smol_rt::TcpConnector;
use aioduct::{OtelMiddleware, SmolClient};
fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        // Set up OpenTelemetry with stdout exporter for demo purposes
        let exporter = opentelemetry_stdout::SpanExporter::default();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(exporter)
            .build();

        opentelemetry::global::set_tracer_provider(provider.clone());

        let client = SmolClient::builder(TcpConnector)
            .middleware(OtelMiddleware::new())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();

        // Each request creates an OpenTelemetry span with HTTP semantic conventions
        let resp = client.get("https://httpbin.org/get")?.send().await?;

        println!("Status: {}", resp.status());
        let _ = resp.text().await?;

        // Spans include: http.method, http.url, http.status_code, etc.
        let resp = client
            .post("https://httpbin.org/post")?
            .body("hello otel")
            .send()
            .await?;

        println!("POST status: {}", resp.status());

        // Shut down the provider to flush spans
        let _ = provider.shutdown();

        println!("\nCheck stdout for exported OpenTelemetry spans");

        Ok(())
    })
}
