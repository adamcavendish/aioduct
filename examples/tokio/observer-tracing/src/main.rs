use std::time::Duration;

use aioduct::TokioClient;
use aioduct::observer::{
    ConnectionEvent, ConnectionPhase, NegotiatedProtocol, PoolOutcome, RequestEvent,
    RequestObserver, RequestPhase, TransferDirection,
};

/// Observer that emits structured tracing events for every request lifecycle phase.
///
/// Each phase maps to a tracing event at an appropriate level:
/// - INFO: request start/complete, transfer complete
/// - DEBUG: connection phases (pool, DNS, TCP, TLS, send)
/// - WARN: failures
/// - TRACE: per-chunk byte transfer
struct TracingObserver;

impl TracingObserver {
    fn protocol_str(p: &NegotiatedProtocol) -> &'static str {
        match p {
            NegotiatedProtocol::Http1 => "h1",
            NegotiatedProtocol::Http2 => "h2",
            NegotiatedProtocol::Http3 => "h3",
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

    fn format_throughput(bytes_per_sec: f32) -> String {
        if bytes_per_sec >= 1_000_000_000.0 {
            format!("{:.1} GB/s", bytes_per_sec / 1_000_000_000.0)
        } else if bytes_per_sec >= 1_000_000.0 {
            format!("{:.1} MB/s", bytes_per_sec / 1_000_000.0)
        } else if bytes_per_sec >= 1_000.0 {
            format!("{:.1} KB/s", bytes_per_sec / 1_000.0)
        } else {
            format!("{:.0} B/s", bytes_per_sec)
        }
    }

    fn format_bytes(bytes: u64) -> String {
        if bytes >= 1_000_000 {
            format!("{:.1} MB", bytes as f64 / 1_000_000.0)
        } else if bytes >= 1_000 {
            format!("{:.1} KB", bytes as f64 / 1_000.0)
        } else {
            format!("{} B", bytes)
        }
    }
}

impl RequestObserver for TracingObserver {
    fn on_event(&self, event: &RequestEvent) {
        let method = &event.method;
        let uri = &event.uri;

        match &event.phase {
            RequestPhase::Started => {
                tracing::info!(%method, %uri, "→ request.start");
            }

            RequestPhase::PoolCheckoutComplete {
                outcome,
                blocked_duration,
            } => {
                tracing::debug!(
                    %method, %uri,
                    outcome = Self::pool_outcome_str(outcome),
                    blocked_us = blocked_duration.as_micros(),
                    "  pool.checkout"
                );
            }

            RequestPhase::DnsResolved { addrs, duration } => {
                let addr_list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
                tracing::debug!(
                    %method, %uri,
                    addrs = %addr_list.join(", "),
                    dns_ms = format_args!("{:.2}", duration.as_secs_f64() * 1000.0),
                    "  dns.resolved"
                );
            }

            RequestPhase::TcpConnected {
                remote_addr,
                duration,
                protocol,
            } => {
                tracing::debug!(
                    %method, %uri,
                    %remote_addr,
                    tcp_ms = format_args!("{:.2}", duration.as_secs_f64() * 1000.0),
                    protocol = Self::protocol_str(protocol),
                    "  tcp.connected"
                );
            }

            RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol,
                peer_certificate_der,
            } => {
                tracing::debug!(
                    %method, %uri,
                    tls_ms = format_args!("{:.2}", duration.as_secs_f64() * 1000.0),
                    alpn = alpn_protocol.as_deref().unwrap_or("none"),
                    has_peer_cert = peer_certificate_der.is_some(),
                    "  tls.handshake"
                );
            }

            RequestPhase::RequestSent { duration, .. } => {
                tracing::debug!(
                    %method, %uri,
                    send_ms = format_args!("{:.2}", duration.as_secs_f64() * 1000.0),
                    "  request.sent"
                );
            }

            RequestPhase::ResponseStarted { waiting_duration } => {
                tracing::debug!(
                    %method, %uri,
                    ttfb_ms = format_args!("{:.2}", waiting_duration.as_secs_f64() * 1000.0),
                    "  response.started (TTFB)"
                );
            }

            RequestPhase::ResponseComplete {
                status,
                protocol,
                total_duration,
            } => {
                tracing::info!(
                    %method, %uri,
                    status = status.as_u16(),
                    protocol = Self::protocol_str(protocol),
                    total_ms = format_args!("{:.2}", total_duration.as_secs_f64() * 1000.0),
                    "← response.complete"
                );
            }

            RequestPhase::Failed {
                error,
                retry,
                elapsed,
            } => {
                tracing::warn!(
                    %method, %uri,
                    %error,
                    ?retry,
                    elapsed_ms = format_args!("{:.2}", elapsed.as_secs_f64() * 1000.0),
                    "✗ request.failed"
                );
            }

            RequestPhase::Redirected { status, from, to } => {
                tracing::info!(
                    %method, %uri,
                    %status,
                    from = %from,
                    to = %to,
                    "↪ redirect"
                );
            }

            RequestPhase::Retrying {
                reason,
                attempt,
                max_retries,
                backoff,
            } => {
                tracing::warn!(
                    %method, %uri,
                    %reason,
                    attempt = format_args!("{attempt}/{max_retries}"),
                    backoff_ms = format_args!("{:.0}", backoff.as_secs_f64() * 1000.0),
                    "↻ retry"
                );
            }

            RequestPhase::BytesTransferred {
                direction,
                chunk_bytes,
                cumulative_bytes,
                elapsed,
            } => {
                tracing::trace!(
                    %method, %uri,
                    direction = Self::direction_str(direction),
                    chunk = Self::format_bytes(*chunk_bytes).as_str(),
                    total = Self::format_bytes(*cumulative_bytes).as_str(),
                    elapsed_ms = format_args!("{:.1}", elapsed.as_secs_f64() * 1000.0),
                    "  transfer.chunk"
                );
            }

            RequestPhase::TransferComplete {
                direction,
                total_bytes,
                transfer_duration,
                throughput_bytes_per_sec,
            } => {
                tracing::info!(
                    %method, %uri,
                    direction = Self::direction_str(direction),
                    total = Self::format_bytes(*total_bytes).as_str(),
                    duration_ms = format_args!("{:.2}", transfer_duration.as_secs_f64() * 1000.0),
                    throughput = Self::format_throughput(*throughput_bytes_per_sec).as_str(),
                    "  transfer.complete"
                );
            }

            RequestPhase::TransferAborted {
                direction,
                bytes_transferred,
                elapsed,
                error,
            } => {
                tracing::warn!(
                    %method, %uri,
                    direction = Self::direction_str(direction),
                    transferred = Self::format_bytes(*bytes_transferred).as_str(),
                    elapsed_ms = format_args!("{:.2}", elapsed.as_secs_f64() * 1000.0),
                    %error,
                    "✗ transfer.aborted"
                );
            }

            RequestPhase::TrailersReceived { headers } => {
                let count = headers.len();
                tracing::info!(
                    %method, %uri,
                    count,
                    "← trailers"
                );
            }
        }
    }

    fn on_connection_event(&self, event: &ConnectionEvent) {
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
                tracing::debug!(
                    %remote_addr,
                    protocol = Self::protocol_str(protocol),
                    sent = Self::format_bytes(*bytes_sent).as_str(),
                    received = Self::format_bytes(*bytes_received).as_str(),
                    age_ms = format_args!("{:.1}", connection_age.as_secs_f64() * 1000.0),
                    requests_served,
                    closed,
                    "  connection.metrics"
                );
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    tracing_subscriber::fmt()
        .with_env_filter("example_observer_tracing=trace")
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .init();

    let client = TokioClient::builder()
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .request_observer(TracingObserver)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // --- 1. HTTPS GET: shows DNS → TCP → TLS → send → TTFB → complete pipeline ---
    tracing::info!("═══ Example 1: HTTPS GET (full connection lifecycle) ═══");
    let resp = client.get("https://httpbin.org/get")?.send().await?;
    let body = resp.bytes().await?;
    tracing::info!(body_len = body.len(), "body consumed\n");

    // --- 2. Second request to same host: shows pool hit (skip DNS/TCP/TLS) ---
    tracing::info!("═══ Example 2: HTTPS GET (pool hit — reused connection) ═══");
    let resp = client.get("https://httpbin.org/headers")?.send().await?;
    let _ = resp.bytes().await?;
    tracing::info!("pool reuse demonstrated\n");

    // --- 3. POST with body: shows bytes_sent tracking ---
    tracing::info!("═══ Example 3: POST with body ═══");
    let resp = client
        .post("https://httpbin.org/post")?
        .body(r#"{"key": "value", "example": "observer tracing demo"}"#)
        .send()
        .await?;
    let _ = resp.bytes().await?;
    tracing::info!("POST complete\n");

    // --- 4. Streaming response: shows BytesTransferred chunks ---
    tracing::info!("═══ Example 4: Streaming download (chunked transfer) ═══");
    let resp = client
        .get("https://httpbin.org/stream-bytes/4096?chunk_size=1024")?
        .send()
        .await?;
    let mut stream = resp.into_bytes_stream();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        total += chunk?.len() as u64;
    }
    tracing::info!(total_bytes = total, "stream consumed\n");

    // --- 5. Redirect chain: shows multiple request cycles ---
    tracing::info!("═══ Example 5: Redirect (multiple request cycles) ═══");
    let resp = client.get("https://httpbin.org/redirect/2")?.send().await?;
    tracing::info!(final_url = %resp.url(), status = resp.status().as_u16(), "redirect resolved");
    let _ = resp.bytes().await?;
    tracing::info!("redirect complete\n");

    // --- 6. Timeout / error: shows Failed event ---
    tracing::info!("═══ Example 6: Request with very short timeout (expect failure) ═══");
    let result = client
        .get("https://httpbin.org/delay/5")?
        .timeout(Duration::from_millis(100))
        .send()
        .await;
    match result {
        Ok(resp) => tracing::info!(status = resp.status().as_u16(), "unexpected success"),
        Err(e) => tracing::info!(error = %e, "expected timeout error"),
    }

    Ok(())
}
