use aioduct::TokioClient;
use std::time::Duration;

/// Application-owned tracing receiver with independent physical connections.
/// Events are low frequency; body sizes arrive only in the finish summary.
struct TracingObserver;

impl aioduct::Observer for TracingObserver {
    type Context = ();
    type Observation = TracingObserver;
    type ConnectionObservation = TracingObserver;
    fn begin(&self, _: aioduct::RequestInfo<'_>, _: Option<()>) -> (aioduct::Interest, Self) {
        (
            aioduct::Interest::REQUEST_START
                | aioduct::Interest::RESPONSE_HEADERS
                | aioduct::Interest::REQUEST_FINISH
                | aioduct::Interest::RESPONSE_BODY_BYTES
                | aioduct::Interest::ATTEMPT
                | aioduct::Interest::CONNECTION_ACQUISITION,
            TracingObserver,
        )
    }
    fn begin_connection(&self) -> (aioduct::Interest, Self) {
        (
            aioduct::Interest::CONNECTION_ESTABLISHMENT | aioduct::Interest::CONNECTION_LIFECYCLE,
            TracingObserver,
        )
    }
}
impl aioduct::RequestObservation for TracingObserver {
    fn on_request_start(&self, event: &aioduct::RequestStart<'_>) {
        tracing::info!(method = %event.method, host = event.initial_uri.host().unwrap_or(""), "http.request.start");
    }
    fn on_response_headers(&self, event: &aioduct::ResponseHeaders) {
        tracing::info!(response = ?event.response, "http.response.headers");
    }
    fn on_attempt(&self, event: &aioduct::AttemptEvent<'_>) {
        tracing::debug!(event = ?event, "http.attempt");
    }
    fn on_connection_acquisition(&self, event: &aioduct::ConnectionAcquisitionEvent<'_>) {
        tracing::debug!(event = ?event, "http.connection.acquisition");
    }
    fn on_request_finish(&self, event: &aioduct::RequestFinish<'_>) {
        tracing::info!(event = ?event, "http.request.finish");
    }
}
impl aioduct::ConnectionObservation for TracingObserver {
    fn on_connection(&self, event: &aioduct::ConnectionObservationEvent<'_>) {
        tracing::debug!(event = ?event, "http.connection");
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
        .observer(TracingObserver)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // --- 1. HTTPS GET: shows DNS → TCP → TLS → headers → body finish pipeline ---
    tracing::info!("═══ Example 1: HTTPS GET (full connection lifecycle) ═══");
    let resp = client.get("https://httpbin.org/get")?.send().await?;
    let body = resp.bytes().await?;
    tracing::info!(body_len = body.len(), "body consumed\n");

    // --- 2. Second request to same host: shows pool hit (skip DNS/TCP/TLS) ---
    tracing::info!("═══ Example 2: HTTPS GET (pool hit — reused connection) ═══");
    let resp = client.get("https://httpbin.org/headers")?.send().await?;
    let _ = resp.bytes().await?;
    tracing::info!("pool reuse demonstrated\n");

    // --- 3. POST with body: reports source-consumption and final summaries ---
    tracing::info!("═══ Example 3: POST with body ═══");
    let resp = client
        .post("https://httpbin.org/post")?
        .body(r#"{"key": "value", "example": "observer tracing demo"}"#)
        .send()
        .await?;
    let _ = resp.bytes().await?;
    tracing::info!("POST complete\n");

    // --- 4. Streaming response: consumes chunks while observation remains low frequency ---
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

    // --- 5. Redirect chain: shows attempts in a single logical request ---
    tracing::info!("═══ Example 5: Redirect (multiple attempts) ═══");
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
