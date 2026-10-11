#[path = "../../smol_threads.rs"]
mod smol_threads;

use aioduct::SmolClient;

fn main() -> Result<(), aioduct::Error> {
    smol_threads::configure();
    smol::block_on(async {
        // Initialize tracing subscriber
        tracing_subscriber::fmt()
            .with_env_filter("aioduct=trace,example_tracing_logs=debug")
            .init();

        tracing::info!("starting tracing example");

        let client = SmolClient::builder()
            .observer(TraceObserver)
            .build()
            .unwrap();

        // The observer emits request events; the tracing feature adds
        // transport diagnostics.
        let resp = client.get("https://httpbin.org/get")?.send().await?;

        tracing::info!(status = %resp.status(), "received response");

        let _body = resp.text().await?;

        // Request that triggers a redirect — visible in traces
        let resp = client.get("https://httpbin.org/redirect/1")?.send().await?;

        tracing::info!(final_url = %resp.url(), "redirect completed");

        // Request that will fail — error event emitted
        let result = client.get("https://httpbin.org/status/500")?.send().await;

        match result {
            Ok(resp) => tracing::warn!(status = %resp.status(), "got error status"),
            Err(e) => tracing::error!(error = %e, "request failed"),
        }

        Ok(())
    })
}

struct TraceObserver;

impl aioduct::Observer for TraceObserver {
    type Context = ();
    type Observation = TraceObserver;
    type ConnectionObservation = TraceObserver;
    fn begin(&self, _: aioduct::RequestInfo<'_>, _: Option<()>) -> (aioduct::Interest, Self) {
        (
            aioduct::Interest::REQUEST_START
                | aioduct::Interest::RESPONSE_HEADERS
                | aioduct::Interest::REQUEST_FINISH
                | aioduct::Interest::RESPONSE_BODY_BYTES
                | aioduct::Interest::ATTEMPT
                | aioduct::Interest::CONNECTION_ACQUISITION,
            TraceObserver,
        )
    }
    fn begin_connection(&self) -> (aioduct::Interest, Self) {
        (
            aioduct::Interest::CONNECTION_ESTABLISHMENT | aioduct::Interest::CONNECTION_LIFECYCLE,
            TraceObserver,
        )
    }
}
impl aioduct::RequestObservation for TraceObserver {
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
impl aioduct::ConnectionObservation for TraceObserver {
    fn on_connection(&self, event: &aioduct::ConnectionObservationEvent<'_>) {
        tracing::debug!(event = ?event, "http.connection");
    }
}
