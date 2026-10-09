use aioduct::CompioClient;

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Initialize tracing subscriber
        tracing_subscriber::fmt()
            .with_env_filter("aioduct=trace,example_tracing_logs=debug")
            .init();

        tracing::info!("starting tracing example");

        let client = CompioClient::builder()
            .request_observer(TraceObserver)
            .build_local()
            .unwrap();

        // The observer emits request events; the tracing feature adds
        // transport diagnostics.
        let resp = client.get_local("https://httpbin.org/get")?.send().await?;

        tracing::info!(status = %resp.status(), "received response");

        let _body = resp.text().await?;

        // Request that triggers a redirect — visible in traces
        let resp = client
            .get_local("https://httpbin.org/redirect/1")?
            .send()
            .await?;

        tracing::info!(final_url = %resp.url(), "redirect completed");

        // Request that will fail — error event emitted
        let result = client
            .get_local("https://httpbin.org/status/500")?
            .send()
            .await;

        match result {
            Ok(resp) => tracing::warn!(status = %resp.status(), "got error status"),
            Err(e) => tracing::error!(error = %e, "request failed"),
        }

        Ok(())
    })
}

struct TraceObserver;

impl aioduct::RequestObserver for TraceObserver {
    fn on_event(&self, event: &aioduct::RequestEvent) {
        tracing::debug!(method = %event.method, host = event.uri.host().unwrap_or(""), phase = ?event.phase, "http.request.event");
    }
    fn on_connection_event(&self, event: &aioduct::ConnectionEvent) {
        tracing::trace!(phase = ?event.phase, "http.connection.event");
    }
}
