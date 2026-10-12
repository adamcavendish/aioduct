use aioduct::{
    AttemptEvent, ChannelIoEvent, ConnectionAcquisitionEvent, Interest, NoopConnectionObservation,
    RequestBodyEvent, RequestFinish, RequestInfo, RequestObservation, RequestStart,
    ResponseBodyEvent, ResponseChannelHandoffEvent, ResponseHeaders, ResponseSseEvent, WasiClient,
};

/// Guest-side observation is intentionally separate from the host transport
/// observer.  WASI owns DNS, connection pooling, negotiated protocol, and wire
/// boundaries, so those native facts are not synthesized in this component.
struct GuestObserver;

struct GuestRequestObservation;

impl aioduct::Observer for GuestObserver {
    type Context = ();
    type Observation = GuestRequestObservation;
    type ConnectionObservation = NoopConnectionObservation;

    fn begin(
        &self,
        request: RequestInfo<'_>,
        _context: Option<Self::Context>,
    ) -> (Interest, Self::Observation) {
        println!("guest begin {} {}", request.method, request.initial_uri);
        (
            Interest::REQUEST_START
                | Interest::RESPONSE_HEADERS
                | Interest::REQUEST_FINISH
                | Interest::ATTEMPT
                | Interest::CONNECTION_ACQUISITION
                | Interest::REQUEST_BODY
                | Interest::RESPONSE_BODY
                | Interest::RESPONSE_BODY_BYTES
                | Interest::SSE_LIFECYCLE
                | Interest::RESPONSE_CHANNEL_HANDOFF
                | Interest::CHANNEL_LIFECYCLE
                | Interest::SSE_STATISTICS
                | Interest::CHANNEL_BYTES,
            GuestRequestObservation,
        )
    }

    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (Interest::EMPTY, NoopConnectionObservation)
    }
}

impl RequestObservation for GuestRequestObservation {
    fn on_request_start(&self, event: &RequestStart<'_>) {
        println!("guest request-start {} {}", event.method, event.initial_uri);
    }

    fn on_response_headers(&self, event: &ResponseHeaders) {
        println!("guest response-headers {:?}", event.response);
    }

    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        println!(
            "guest request-finish reason={:?} response={:?} attempts={} original_data_bytes={:?}",
            event.reason, event.response, event.attempt_count, event.original_data_bytes
        );
    }

    fn on_attempt(&self, event: &AttemptEvent<'_>) {
        println!("guest attempt {event:?}");
    }

    fn on_connection_acquisition(&self, event: &ConnectionAcquisitionEvent<'_>) {
        println!("guest acquisition {event:?}");
    }

    fn on_request_body(&self, event: &RequestBodyEvent<'_>) {
        println!("guest request-body {event:?}");
    }

    fn on_response_body(&self, event: &ResponseBodyEvent<'_>) {
        println!("guest response-body {event:?}");
    }

    fn on_response_sse(&self, event: ResponseSseEvent<'_>) {
        println!("guest sse {event:?}");
    }

    fn on_response_channel_handoff(&self, event: &ResponseChannelHandoffEvent<'_>) {
        println!("guest channel-handoff {event:?}");
    }

    fn on_channel_io(&self, event: &ChannelIoEvent<'_>) {
        println!("guest channel {event:?}");
    }
}

fn main() {
    let client = WasiClient::builder()
        .user_agent("aioduct-wasi-p2-demo/0.2")
        .observer(GuestObserver)
        .build()
        .unwrap();
    let base_url = std::env::var("AIODUCT_WASI_DEMO_URL")
        .expect("AIODUCT_WASI_DEMO_URL must be provided by the host example");

    // Simple GET
    let get_url = format!("{base_url}/get");
    println!("=== GET {get_url} ===");
    match client.get(&get_url) {
        Ok(req) => match req.send() {
            Ok(resp) => {
                println!("Status: {}", resp.status());
                for (name, value) in resp.headers() {
                    if let Ok(v) = value.to_str() {
                        println!("  {name}: {v}");
                    }
                }
                match resp.text() {
                    Ok(body) => println!("\nBody:\n{body}"),
                    Err(e) => eprintln!("Body read error: {e}"),
                }
            }
            Err(e) => eprintln!("Request error: {e}"),
        },
        Err(e) => eprintln!("Build error: {e}"),
    }

    // POST JSON
    let post_url = format!("{base_url}/post");
    println!("\n=== POST {post_url} ===");
    let payload = serde_json::json!({
        "message": "hello from WASI",
        "runtime": "wasm32-wasip2"
    });
    match client.post(&post_url) {
        Ok(req) => match req.json(&payload) {
            Ok(req) => match req.send() {
                Ok(resp) => {
                    println!("Status: {}", resp.status());
                    match resp.json::<serde_json::Value>() {
                        Ok(json) => {
                            println!(
                                "Body:\n{}",
                                serde_json::to_string_pretty(&json).unwrap_or_default()
                            );
                        }
                        Err(e) => eprintln!("JSON parse error: {e}"),
                    }
                }
                Err(e) => eprintln!("Request error: {e}"),
            },
            Err(e) => eprintln!("JSON serialize error: {e}"),
        },
        Err(e) => eprintln!("Build error: {e}"),
    }

    // Error handling
    let not_found_url = format!("{base_url}/status/404");
    println!("\n=== GET {not_found_url} ===");
    match client.get(&not_found_url) {
        Ok(req) => match req.send() {
            Ok(resp) => match resp.error_for_status() {
                Ok(_) => println!("Unexpected success"),
                Err(e) => println!("Expected error: {e}"),
            },
            Err(e) => eprintln!("Request error: {e}"),
        },
        Err(e) => eprintln!("Build error: {e}"),
    }
}
