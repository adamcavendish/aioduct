use aioduct::TokioClient;
use opentelemetry::KeyValue;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer, TracerProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::time::Duration;

/// The application chooses and owns its OTel integration.
struct OtelObserver;
struct OtelRequestObservation(opentelemetry::Context);
struct OtelConnectionObservation(opentelemetry::Context);

impl aioduct::Observer for OtelObserver {
    type Context = opentelemetry::Context;
    type Observation = OtelRequestObservation;
    type ConnectionObservation = OtelConnectionObservation;
    fn begin(
        &self,
        _: aioduct::RequestInfo<'_>,
        context: Option<Self::Context>,
    ) -> (aioduct::Interest, Self::Observation) {
        (
            aioduct::Interest::REQUEST_START
                | aioduct::Interest::RESPONSE_HEADERS
                | aioduct::Interest::REQUEST_FINISH
                | aioduct::Interest::RESPONSE_BODY_BYTES
                | aioduct::Interest::ATTEMPT
                | aioduct::Interest::CONNECTION_ACQUISITION,
            OtelRequestObservation(context.unwrap_or_else(opentelemetry::Context::current)),
        )
    }
    fn begin_connection(&self) -> (aioduct::Interest, Self::ConnectionObservation) {
        // Physical connections outlive requests and do not borrow a request span.
        let tracer = opentelemetry::global::tracer("aioduct-connection-example");
        let span = tracer.start_with_context("aioduct.connection", &opentelemetry::Context::new());
        (
            aioduct::Interest::CONNECTION_LIFECYCLE | aioduct::Interest::CONNECTION_ESTABLISHMENT,
            OtelConnectionObservation(opentelemetry::Context::new().with_span(span)),
        )
    }
}
impl aioduct::RequestObservation for OtelRequestObservation {
    fn on_request_start(&self, event: &aioduct::RequestStart<'_>) {
        self.0.span().add_event(
            "request.start",
            vec![KeyValue::new(
                "http.request.method",
                event.method.to_string(),
            )],
        );
    }
    fn on_response_headers(&self, event: &aioduct::ResponseHeaders) {
        self.0.span().add_event(
            "response.headers",
            vec![KeyValue::new(
                "http.response.status_code",
                event.response.status.as_u16() as i64,
            )],
        );
    }
    fn on_attempt(&self, event: &aioduct::AttemptEvent<'_>) {
        self.0.span().add_event(
            "request.attempt",
            vec![KeyValue::new("event", format!("{event:?}"))],
        );
    }
    fn on_connection_acquisition(&self, event: &aioduct::ConnectionAcquisitionEvent<'_>) {
        self.0.span().add_event(
            "connection.acquisition",
            vec![KeyValue::new("event", format!("{event:?}"))],
        );
    }
    fn on_request_finish(&self, event: &aioduct::RequestFinish<'_>) {
        self.0.span().add_event(
            "request.finish",
            vec![KeyValue::new("event", format!("{event:?}"))],
        );
        if let aioduct::RequestFinishReason::Failed { error } = event.reason {
            self.0.span().record_error(error);
            self.0.span().set_status(Status::error(error.to_string()));
        }
    }
}
impl aioduct::ConnectionObservation for OtelConnectionObservation {
    fn on_connection(&self, event: &aioduct::ConnectionObservationEvent<'_>) {
        self.0.span().add_event(
            "connection.event",
            vec![KeyValue::new("event", format!("{event:?}"))],
        );
        if matches!(
            event,
            aioduct::ConnectionObservationEvent::Closed { .. }
                | aioduct::ConnectionObservationEvent::Detached { .. }
                | aioduct::ConnectionObservationEvent::EstablishmentFailed { .. }
                | aioduct::ConnectionObservationEvent::EstablishmentCancelled { .. }
        ) {
            self.0.span().end();
        }
    }
}

async fn traced_request<T>(
    client: &TokioClient<OtelObserver>,
    tracer: &T,
    span_name: &str,
    url: &str,
) -> Result<(), aioduct::Error>
where
    T: Tracer,
    T::Span: Send + Sync + 'static,
{
    let span = tracer
        .span_builder(span_name.to_string())
        .with_kind(SpanKind::Client)
        .with_attributes(vec![
            KeyValue::new("http.request.method", "GET"),
            KeyValue::new("url.full", url.to_string()),
        ])
        .start(tracer);
    let cx = opentelemetry::Context::current().with_span(span);
    let _guard = cx.attach();

    let resp = client.get(url)?.send().await?;
    let body = resp.bytes().await?;
    println!("  {span_name}: {} bytes", body.len());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up OTel with stdout exporter (JSON spans printed to terminal)
    let exporter = opentelemetry_stdout::SpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());

    let tracer = provider.tracer("aioduct-observer-otel-example");

    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .observer(OtelObserver)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    println!("=== OTel Observer Example ===\n");
    println!("Each request creates an OTel span with per-phase events.\n");

    // 1. Fresh HTTPS connection — full DNS → TCP → TLS → HTTP pipeline
    println!("1. HTTPS GET (fresh connection — DNS, TCP, TLS, HTTP):");
    traced_request(&client, &tracer, "GET /get", "https://httpbin.org/get").await?;

    // 2. Pool hit — reuses existing connection (no DNS/TCP/TLS events)
    println!("\n2. HTTPS GET (pool hit — connection reused):");
    traced_request(
        &client,
        &tracer,
        "GET /headers",
        "https://httpbin.org/headers",
    )
    .await?;

    // 3. POST with body — reports final byte summaries
    println!("\n3. POST with body:");
    {
        let span = tracer
            .span_builder("POST /post".to_string())
            .with_kind(SpanKind::Client)
            .start(&tracer);
        let cx = opentelemetry::Context::current().with_span(span);
        let _guard = cx.attach();

        let resp = client
            .post("https://httpbin.org/post")?
            .body(r#"{"observer": "otel", "example": "thorough"}"#)
            .send()
            .await?;
        let body = resp.bytes().await?;
        println!("  POST /post: {} bytes", body.len());
    }

    // 4. Streaming — shows BytesTransferred events per chunk
    println!("\n4. Streaming download (per-chunk events):");
    {
        let span = tracer
            .span_builder("GET /stream-bytes".to_string())
            .with_kind(SpanKind::Client)
            .start(&tracer);
        let cx = opentelemetry::Context::current().with_span(span);
        let _guard = cx.attach();

        let resp = client
            .get("https://httpbin.org/stream-bytes/8192?chunk_size=2048")?
            .send()
            .await?;
        let mut stream = resp.into_bytes_stream();
        let mut chunks = 0u32;
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let data = chunk?;
            total += data.len() as u64;
            chunks += 1;
        }
        println!("  Stream: {total} bytes in {chunks} chunks");
    }

    // 5. Timeout error — shows Failed event with error details
    println!("\n5. Timeout (expect Failed event):");
    {
        let span = tracer
            .span_builder("GET /delay (timeout)".to_string())
            .with_kind(SpanKind::Client)
            .start(&tracer);
        let cx = opentelemetry::Context::current().with_span(span);
        let _guard = cx.attach();

        let result = client
            .get("https://httpbin.org/delay/10")?
            .timeout(Duration::from_millis(200))
            .send()
            .await;
        match result {
            Ok(resp) => println!("  Unexpected success: {}", resp.status()),
            Err(e) => println!("  Expected error: {e}"),
        }
    }

    // Flush all spans
    println!("\n=== Shutting down OTel provider (flushing spans) ===\n");
    let _ = provider.shutdown();

    println!("Check stdout above for JSON-formatted OTel spans with per-phase events.");
    println!(
        "Each span contains timeline events: acquisition → headers → request finish; independent connection events."
    );

    Ok(())
}
