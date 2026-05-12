use aioduct::HttpEngine;
use aioduct::observer::{RequestEvent, RequestObserver, RequestPhase};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use opentelemetry::KeyValue;
use opentelemetry::trace::{TraceContextExt, Tracer};
use opentelemetry_sdk::trace::SdkTracerProvider;

struct OtelObserver;

impl RequestObserver for OtelObserver {
    fn on_event(&self, event: &RequestEvent) {
        let cx = opentelemetry::Context::current();
        let span = cx.span();

        match &event.phase {
            RequestPhase::Started => {
                span.add_event(
                    "http.request.start",
                    vec![
                        KeyValue::new("http.method", event.method.to_string()),
                        KeyValue::new("http.url", event.uri.to_string()),
                    ],
                );
            }
            RequestPhase::DnsResolved { addrs, duration } => {
                span.add_event(
                    "dns.resolved",
                    vec![
                        KeyValue::new("dns.duration_ms", duration.as_millis() as i64),
                        KeyValue::new("dns.addrs_count", addrs.len() as i64),
                    ],
                );
            }
            RequestPhase::TcpConnected {
                remote_addr,
                duration,
                protocol,
            } => {
                span.add_event(
                    "tcp.connected",
                    vec![
                        KeyValue::new("net.peer.addr", remote_addr.to_string()),
                        KeyValue::new("tcp.duration_ms", duration.as_millis() as i64),
                        KeyValue::new("http.flavor", format!("{protocol:?}")),
                    ],
                );
            }
            RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol,
                ..
            } => {
                let mut attrs = vec![KeyValue::new(
                    "tls.duration_ms",
                    duration.as_millis() as i64,
                )];
                if let Some(alpn) = alpn_protocol {
                    attrs.push(KeyValue::new("tls.alpn", alpn.clone()));
                }
                span.add_event("tls.handshake.complete", attrs);
            }
            RequestPhase::ResponseComplete {
                status,
                protocol,
                total_duration,
            } => {
                span.add_event(
                    "http.response.complete",
                    vec![
                        KeyValue::new("http.status_code", status.as_u16() as i64),
                        KeyValue::new("http.flavor", format!("{protocol:?}")),
                        KeyValue::new("http.duration_ms", total_duration.as_millis() as i64),
                    ],
                );
            }
            RequestPhase::Failed {
                error,
                will_retry,
                elapsed,
            } => {
                span.add_event(
                    "http.request.failed",
                    vec![
                        KeyValue::new("error.message", error.clone()),
                        KeyValue::new("http.retry", *will_retry),
                        KeyValue::new("http.elapsed_ms", elapsed.as_millis() as i64),
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
                        KeyValue::new("transfer.direction", format!("{direction:?}")),
                        KeyValue::new("transfer.bytes", *total_bytes as i64),
                        KeyValue::new("transfer.duration_ms", transfer_duration.as_millis() as i64),
                        KeyValue::new(
                            "transfer.throughput_mbps",
                            *throughput_bytes_per_sec / 1_000_000.0,
                        ),
                    ],
                );
            }
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let exporter = opentelemetry_stdout::SpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .request_observer(OtelObserver)
        .build();

    // Create a root span — observer events attach to the active span context
    let tracer = opentelemetry::global::tracer("aioduct-observer-example");
    let span = tracer.start("http-request");
    let cx = opentelemetry::Context::current().with_span(span);
    let _guard = cx.attach();

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Status: {}", resp.status());
    let body = resp.bytes().await?;
    println!("Body length: {} bytes", body.len());

    drop(_guard);
    let _ = provider.shutdown();

    println!("\nCheck stdout for exported OpenTelemetry spans with per-phase events");

    Ok(())
}
