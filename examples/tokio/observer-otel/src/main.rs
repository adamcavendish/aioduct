use std::time::Duration;

use aioduct::TokioClient;
use aioduct::observer::{
    ConnectionEvent, ConnectionPhase, NegotiatedProtocol, PoolOutcome, RequestEvent,
    RequestObserver, RequestPhase, TransferDirection,
};
use opentelemetry::KeyValue;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer, TracerProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;

/// Observer that records per-phase OTel span events with semantic HTTP attributes.
///
/// Attaches events to the currently-active span context, giving sub-request
/// visibility that middleware-level instrumentation cannot provide.
struct OtelObserver;

impl OtelObserver {
    fn protocol_str(p: &NegotiatedProtocol) -> &'static str {
        match p {
            NegotiatedProtocol::Http1 => "1.1",
            NegotiatedProtocol::Http2 => "2",
            NegotiatedProtocol::Http3 => "3",
        }
    }

    fn pool_outcome_str(o: &PoolOutcome) -> &'static str {
        match o {
            PoolOutcome::Hit => "hit",
            PoolOutcome::Coalesced => "coalesced",
            PoolOutcome::Miss => "miss",
            PoolOutcome::StaleRetry => "stale_retry",
        }
    }

    fn direction_str(d: &TransferDirection) -> &'static str {
        match d {
            TransferDirection::Upload => "upload",
            TransferDirection::Download => "download",
        }
    }
}

impl RequestObserver for OtelObserver {
    fn on_event(&self, event: &RequestEvent) {
        let cx = opentelemetry::Context::current();
        let span = cx.span();

        match &event.phase {
            RequestPhase::Started => {
                span.add_event(
                    "http.request.start",
                    vec![
                        KeyValue::new("http.request.method", event.method.to_string()),
                        KeyValue::new("url.full", event.uri.to_string()),
                    ],
                );
            }

            RequestPhase::PoolCheckoutComplete {
                outcome,
                blocked_duration,
            } => {
                span.add_event(
                    "http.connection.pool_checkout",
                    vec![
                        KeyValue::new("pool.outcome", Self::pool_outcome_str(outcome)),
                        KeyValue::new(
                            "pool.blocked_duration_ms",
                            blocked_duration.as_secs_f64() * 1000.0,
                        ),
                    ],
                );
            }

            RequestPhase::DnsResolved { addrs, duration } => {
                span.add_event(
                    "dns.resolve",
                    vec![
                        KeyValue::new("dns.resolved_count", addrs.len() as i64),
                        KeyValue::new(
                            "dns.resolved_addrs",
                            addrs
                                .iter()
                                .map(|a| a.to_string())
                                .collect::<Vec<_>>()
                                .join(","),
                        ),
                        KeyValue::new("dns.duration_ms", duration.as_secs_f64() * 1000.0),
                    ],
                );
            }

            RequestPhase::TcpConnected {
                remote_addr,
                duration,
                protocol,
            } => {
                span.add_event(
                    "net.connect",
                    vec![
                        KeyValue::new("net.peer.addr", remote_addr.ip().to_string()),
                        KeyValue::new("net.peer.port", remote_addr.port() as i64),
                        KeyValue::new("net.connect_duration_ms", duration.as_secs_f64() * 1000.0),
                        KeyValue::new("http.flavor", Self::protocol_str(protocol)),
                    ],
                );
            }

            RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol,
                peer_certificate_der,
            } => {
                let mut attrs = vec![
                    KeyValue::new("tls.handshake_duration_ms", duration.as_secs_f64() * 1000.0),
                    KeyValue::new("tls.established", true),
                ];
                if let Some(alpn) = alpn_protocol {
                    attrs.push(KeyValue::new("tls.alpn_protocol", alpn.clone()));
                }
                if let Some(cert) = peer_certificate_der {
                    attrs.push(KeyValue::new(
                        "tls.peer_certificate_size",
                        cert.len() as i64,
                    ));
                }
                span.add_event("tls.handshake", attrs);
            }

            RequestPhase::RequestSent { duration } => {
                span.add_event(
                    "http.request.sent",
                    vec![KeyValue::new(
                        "http.send_duration_ms",
                        duration.as_secs_f64() * 1000.0,
                    )],
                );
            }

            RequestPhase::ResponseStarted { waiting_duration } => {
                span.add_event(
                    "http.response.first_byte",
                    vec![KeyValue::new(
                        "http.time_to_first_byte_ms",
                        waiting_duration.as_secs_f64() * 1000.0,
                    )],
                );
            }

            RequestPhase::ResponseComplete {
                status,
                protocol,
                total_duration,
            } => {
                span.add_event(
                    "http.response.complete",
                    vec![
                        KeyValue::new("http.response.status_code", status.as_u16() as i64),
                        KeyValue::new("http.flavor", Self::protocol_str(protocol)),
                        KeyValue::new(
                            "http.total_duration_ms",
                            total_duration.as_secs_f64() * 1000.0,
                        ),
                    ],
                );
            }

            RequestPhase::Failed {
                error,
                will_retry,
                elapsed,
            } => {
                span.add_event(
                    "http.request.error",
                    vec![
                        KeyValue::new("error.type", error.clone()),
                        KeyValue::new("http.will_retry", *will_retry),
                        KeyValue::new("http.elapsed_ms", elapsed.as_secs_f64() * 1000.0),
                    ],
                );
                span.set_status(Status::error(error.clone()));
            }

            RequestPhase::BytesTransferred {
                direction,
                chunk_bytes,
                cumulative_bytes,
                elapsed,
            } => {
                span.add_event(
                    "http.transfer.chunk",
                    vec![
                        KeyValue::new("transfer.direction", Self::direction_str(direction)),
                        KeyValue::new("transfer.chunk_bytes", *chunk_bytes as i64),
                        KeyValue::new("transfer.cumulative_bytes", *cumulative_bytes as i64),
                        KeyValue::new("transfer.elapsed_ms", elapsed.as_secs_f64() * 1000.0),
                    ],
                );
            }

            RequestPhase::TransferComplete {
                direction,
                total_bytes,
                transfer_duration,
                throughput_bytes_per_sec,
            } => {
                span.add_event(
                    "http.transfer.complete",
                    vec![
                        KeyValue::new("transfer.direction", Self::direction_str(direction)),
                        KeyValue::new("transfer.total_bytes", *total_bytes as i64),
                        KeyValue::new(
                            "transfer.duration_ms",
                            transfer_duration.as_secs_f64() * 1000.0,
                        ),
                        KeyValue::new(
                            "transfer.throughput_bytes_per_sec",
                            *throughput_bytes_per_sec as f64,
                        ),
                    ],
                );
            }

            RequestPhase::TransferAborted {
                direction,
                bytes_transferred,
                elapsed,
                error,
            } => {
                span.add_event(
                    "http.transfer.aborted",
                    vec![
                        KeyValue::new("transfer.direction", Self::direction_str(direction)),
                        KeyValue::new("transfer.bytes_transferred", *bytes_transferred as i64),
                        KeyValue::new("transfer.elapsed_ms", elapsed.as_secs_f64() * 1000.0),
                        KeyValue::new("error.type", error.clone()),
                    ],
                );
                span.set_status(Status::error(error.clone()));
            }
        }
    }

    fn on_connection_event(&self, event: &ConnectionEvent) {
        let cx = opentelemetry::Context::current();
        let span = cx.span();

        match &event.phase {
            ConnectionPhase::Metrics {
                remote_addr,
                protocol,
                bytes_sent,
                bytes_received,
                connection_age,
                requests_served,
                closed,
            } => {
                span.add_event(
                    "http.connection.metrics",
                    vec![
                        KeyValue::new("net.peer.addr", remote_addr.to_string()),
                        KeyValue::new("http.flavor", Self::protocol_str(protocol)),
                        KeyValue::new("connection.bytes_sent", *bytes_sent as i64),
                        KeyValue::new("connection.bytes_received", *bytes_received as i64),
                        KeyValue::new("connection.age_ms", connection_age.as_secs_f64() * 1000.0),
                        KeyValue::new("connection.requests_served", *requests_served as i64),
                        KeyValue::new("connection.closed", *closed),
                    ],
                );
            }
        }
    }
}

/// Helper to execute a request inside a named OTel span.
async fn traced_request<T>(
    client: &TokioClient,
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
        .request_observer(OtelObserver)
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

    // 3. POST with body — shows bytes_sent in ConnectionMetrics
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
        "Each span contains timeline events: pool → dns → tcp → tls → send → ttfb → complete."
    );

    Ok(())
}
