use std::time::Duration;

use aioduct::TokioClient;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer, TracerProvider};
use opentelemetry::{Context, KeyValue};
use opentelemetry_http::HeaderInjector;
use opentelemetry_sdk::propagation::TraceContextPropagator;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(opentelemetry_stdout::SpanExporter::default())
        .build();
    let tracer = provider.tracer("example-tokio-otel-spans");
    let span = tracer
        .span_builder("HTTP GET")
        .with_kind(SpanKind::Client)
        .start(&tracer);
    let context = Context::current_with_span(span);

    // Capture and inject context in the application before handing off the request.
    // No thread-local span guard is held across an await.
    let mut headers = http::HeaderMap::new();
    TraceContextPropagator::new().inject_context(&context, &mut HeaderInjector(&mut headers));
    let client = TokioClient::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let result = async {
        let response = client
            .get("https://httpbin.org/get")?
            .headers(headers)
            .send()
            .await?;
        context.span().set_attribute(KeyValue::new(
            "http.response.status_code",
            response.status().as_u16() as i64,
        ));
        println!("Status: {}", response.status());
        response.text().await
    }
    .await;
    if let Err(error) = &result {
        context.span().set_status(Status::error(error.to_string()));
    }
    // Application owns the span, including body consumption and errors.
    context.span().end();
    let _ = provider.shutdown();
    result.map(|_| ())
}
