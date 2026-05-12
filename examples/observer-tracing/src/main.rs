use aioduct::HttpEngine;
use aioduct::observer::{RequestEvent, RequestObserver, RequestPhase};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

struct TracingObserver;

impl RequestObserver for TracingObserver {
    fn on_event(&self, event: &RequestEvent) {
        match &event.phase {
            RequestPhase::Started => {
                tracing::info!(
                    method = %event.method,
                    uri = %event.uri,
                    "request.start"
                );
            }
            RequestPhase::PoolCheckoutComplete {
                outcome,
                blocked_duration,
            } => {
                tracing::debug!(
                    ?outcome,
                    blocked_ms = blocked_duration.as_millis(),
                    "pool.checkout"
                );
            }
            RequestPhase::DnsResolved { addrs, duration } => {
                tracing::debug!(?addrs, dns_ms = duration.as_millis(), "dns.resolved");
            }
            RequestPhase::TcpConnected {
                remote_addr,
                duration,
                protocol,
            } => {
                tracing::debug!(
                    %remote_addr,
                    tcp_ms = duration.as_millis(),
                    ?protocol,
                    "tcp.connected"
                );
            }
            RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol,
                ..
            } => {
                tracing::debug!(
                    tls_ms = duration.as_millis(),
                    alpn = ?alpn_protocol,
                    "tls.handshake.complete"
                );
            }
            RequestPhase::RequestSent { duration } => {
                tracing::debug!(send_ms = duration.as_millis(), "request.sent");
            }
            RequestPhase::ResponseStarted { waiting_duration } => {
                tracing::debug!(ttfb_ms = waiting_duration.as_millis(), "response.started");
            }
            RequestPhase::ResponseComplete {
                status,
                protocol,
                total_duration,
            } => {
                tracing::info!(
                    status = status.as_u16(),
                    ?protocol,
                    total_ms = total_duration.as_millis(),
                    "response.complete"
                );
            }
            RequestPhase::Failed {
                error,
                will_retry,
                elapsed,
            } => {
                tracing::warn!(
                    %error,
                    will_retry,
                    elapsed_ms = elapsed.as_millis(),
                    "request.failed"
                );
            }
            RequestPhase::BytesTransferred {
                direction,
                chunk_bytes,
                cumulative_bytes,
                elapsed,
            } => {
                tracing::trace!(
                    ?direction,
                    chunk_bytes,
                    cumulative_bytes,
                    elapsed_ms = elapsed.as_millis(),
                    "transfer.chunk"
                );
            }
            RequestPhase::TransferComplete {
                direction,
                total_bytes,
                transfer_duration,
                throughput_bytes_per_sec,
            } => {
                tracing::info!(
                    ?direction,
                    total_bytes,
                    duration_ms = transfer_duration.as_millis(),
                    throughput_mbps = throughput_bytes_per_sec / 1_000_000.0,
                    "transfer.complete"
                );
            }
            RequestPhase::ConnectionMetrics {
                remote_addr,
                protocol,
                bytes_sent,
                bytes_received,
                connection_age,
                requests_served,
                closed,
            } => {
                tracing::debug!(
                    %remote_addr,
                    ?protocol,
                    bytes_sent,
                    bytes_received,
                    age_ms = connection_age.as_millis(),
                    requests_served,
                    closed,
                    "connection.metrics"
                );
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    tracing_subscriber::fmt()
        .with_env_filter("example_observer_tracing=trace")
        .init();

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .request_observer(TracingObserver)
        .build();

    tracing::info!("sending request to httpbin.org");

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    let body = resp.bytes().await?;

    tracing::info!(body_len = body.len(), "done");

    Ok(())
}
