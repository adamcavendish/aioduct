#![cfg(feature = "tokio")]

mod common;
use common::*;

use std::sync::Mutex;

use aioduct::observer::{RequestEvent, RequestObserver, RequestPhase};

#[derive(Default, Clone)]
struct RecordingObserver {
    events: Arc<Mutex<Vec<RequestPhase>>>,
}

impl RequestObserver for RecordingObserver {
    fn on_event(&self, event: &RequestEvent) {
        self.events.lock().unwrap().push(event.phase.clone());
    }
}

impl RecordingObserver {
    fn phases(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|p| match p {
                RequestPhase::Started => "Started".into(),
                RequestPhase::PoolCheckoutComplete { outcome, .. } => {
                    format!("PoolCheckoutComplete({outcome:?})")
                }
                RequestPhase::DnsResolved { .. } => "DnsResolved".into(),
                RequestPhase::TcpConnected { .. } => "TcpConnected".into(),
                RequestPhase::TlsHandshakeComplete { .. } => "TlsHandshakeComplete".into(),
                RequestPhase::RequestSent { .. } => "RequestSent".into(),
                RequestPhase::ResponseStarted { .. } => "ResponseStarted".into(),
                RequestPhase::ResponseComplete { .. } => "ResponseComplete".into(),
                RequestPhase::Failed { .. } => "Failed".into(),
                RequestPhase::BytesTransferred { .. } => "BytesTransferred".into(),
                RequestPhase::TransferComplete { .. } => "TransferComplete".into(),
                RequestPhase::ConnectionMetrics { .. } => "ConnectionMetrics".into(),
            })
            .collect()
    }
}

#[tokio::test]
async fn observer_fires_full_lifecycle_on_fresh_connection() {
    let addr = start_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let _body = resp.bytes().await.unwrap();

    let phases = obs.phases();
    assert!(
        phases.contains(&"Started".to_string()),
        "phases: {phases:?}"
    );
    assert!(
        phases.contains(&"PoolCheckoutComplete(Miss)".to_string()),
        "phases: {phases:?}"
    );
    assert!(
        phases.contains(&"DnsResolved".to_string()),
        "phases: {phases:?}"
    );
    assert!(
        phases.contains(&"TcpConnected".to_string()),
        "phases: {phases:?}"
    );
    assert!(
        phases.contains(&"RequestSent".to_string()),
        "phases: {phases:?}"
    );
    assert!(
        phases.contains(&"ResponseStarted".to_string()),
        "phases: {phases:?}"
    );
    assert!(
        phases.contains(&"ResponseComplete".to_string()),
        "phases: {phases:?}"
    );
    assert!(
        phases.contains(&"TransferComplete".to_string()),
        "phases: {phases:?}"
    );
}

#[tokio::test]
async fn observer_fires_pool_hit_on_reused_connection() {
    let addr = start_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .build();

    // First request — fresh connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    // Clear events
    obs.events.lock().unwrap().clear();

    // Second request — should reuse pooled connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    let phases = obs.phases();
    assert!(
        phases.contains(&"PoolCheckoutComplete(Hit)".to_string()),
        "Expected pool hit on second request, got: {phases:?}"
    );
    // Should NOT have DNS or TCP for a pool hit
    assert!(
        !phases.contains(&"DnsResolved".to_string()),
        "Should not resolve DNS on pool hit: {phases:?}"
    );
    assert!(
        !phases.contains(&"TcpConnected".to_string()),
        "Should not TCP connect on pool hit: {phases:?}"
    );
}

#[tokio::test]
async fn observer_bytes_transferred_fires_during_streaming() {
    let addr = start_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);

    let mut stream = resp.into_bytes_stream();
    let mut total = 0u64;
    while let Some(chunk) = stream.next().await {
        total += chunk.unwrap().len() as u64;
    }
    assert!(total > 0);

    let phases = obs.phases();
    assert!(
        phases.contains(&"BytesTransferred".to_string()),
        "Expected BytesTransferred during streaming: {phases:?}"
    );
    assert!(
        phases.contains(&"TransferComplete".to_string()),
        "Expected TransferComplete after stream ends: {phases:?}"
    );
}

#[tokio::test]
async fn observer_captures_method_and_uri() {
    let addr = start_server().await;

    let events: Arc<Mutex<Vec<(http::Method, http::Uri)>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    struct UriCapture(Arc<Mutex<Vec<(http::Method, http::Uri)>>>);
    impl RequestObserver for UriCapture {
        fn on_event(&self, event: &RequestEvent) {
            if matches!(event.phase, RequestPhase::Started) {
                self.0
                    .lock()
                    .unwrap()
                    .push((event.method.clone(), event.uri.clone()));
            }
        }
    }

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(UriCapture(events_clone))
        .build();

    let resp = client
        .get(&format!("http://{addr}/test-path"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].0, http::Method::GET);
    assert!(captured[0].1.to_string().contains("/test-path"));
}

#[tokio::test]
async fn observer_no_events_when_not_configured() {
    let addr = start_server().await;

    // Client without observer — should not panic or error
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector).build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
