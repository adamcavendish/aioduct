#![cfg(feature = "tokio")]

use std::sync::Arc;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server;

use std::sync::Mutex;

use aioduct::observer::{
    ConnectionEvent, ConnectionPhase, NegotiatedProtocol, RequestEvent, RequestObserver,
    RequestPhase,
};

#[derive(Default, Clone)]
struct RecordingObserver {
    events: Arc<Mutex<Vec<RequestPhase>>>,
    connection_events: Arc<Mutex<Vec<ConnectionPhase>>>,
}

impl RequestObserver for RecordingObserver {
    fn on_event(&self, event: &RequestEvent) {
        self.events.lock().unwrap().push(event.phase.clone());
    }

    fn on_connection_event(&self, event: &ConnectionEvent) {
        self.connection_events
            .lock()
            .unwrap()
            .push(event.phase.clone());
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
                RequestPhase::TransferAborted { .. } => "TransferAborted".into(),
                RequestPhase::Redirected { .. } => "Redirected".into(),
                RequestPhase::Retrying { .. } => "Retrying".into(),
                RequestPhase::TrailersReceived { .. } => "TrailersReceived".into(),
            })
            .collect()
    }

    fn has_connection_metrics(&self) -> bool {
        !self.connection_events.lock().unwrap().is_empty()
    }

    fn connection_phases(&self) -> Vec<ConnectionPhase> {
        self.connection_events.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn observer_fires_full_lifecycle_on_fresh_connection() {
    let (addr, _counter) = h1_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .build()
        .unwrap();

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
async fn observer_connection_metrics_fires_on_checkin() {
    let (addr, _counter) = h1_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    assert!(
        obs.has_connection_metrics(),
        "Expected ConnectionMetrics on pool checkin"
    );
}

#[tokio::test]
async fn observer_connection_metrics_include_pool_checkin_totals() {
    let (addr, _counter) = h1_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .build()
        .unwrap();

    let resp = client
        .post(&format!("http://{addr}/metrics"))
        .unwrap()
        .body("payload")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    let phases = obs.connection_phases();
    let metrics = phases
        .first()
        .map(|phase| {
            let ConnectionPhase::Metrics {
                protocol,
                bytes_sent,
                bytes_received,
                connection_age,
                requests_served,
                closed,
                ..
            } = phase;
            (
                *protocol,
                *bytes_sent,
                *bytes_received,
                *connection_age,
                *requests_served,
                *closed,
            )
        })
        .expect("expected connection metrics event");

    assert_eq!(metrics.0, NegotiatedProtocol::Http1);
    assert_eq!(metrics.1, 7, "request body bytes should be counted");
    assert_eq!(metrics.2, 13, "response Content-Length should be counted");
    assert!(
        metrics.3 <= std::time::Duration::from_secs(5),
        "connection age should be a bounded elapsed duration, got {:?}",
        metrics.3
    );
    assert_eq!(metrics.4, 1, "first checkin should report one request");
    assert!(
        !metrics.5,
        "pool checkin metrics should not mark closed=true"
    );
}

#[tokio::test]
async fn pool_stats_report_hit_miss_and_inventory() {
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    let initial = client.pool_stats();
    assert_eq!(initial.checkout_hits, 0);
    assert_eq!(initial.checkout_misses, 0);
    assert_eq!(initial.idle_pool_entries, 0);
    assert_eq!(initial.checked_out_pool_handles, 0);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    let after_first = client.pool_stats();
    assert_eq!(after_first.checkout_misses, 1);
    assert_eq!(after_first.checkout_hits, 0);
    assert_eq!(after_first.idle_pool_entries, 1);
    assert_eq!(after_first.checked_out_pool_handles, 0);
    assert_eq!(after_first.hosts.len(), 1);
    assert_eq!(after_first.hosts[0].scheme, "http");
    assert_eq!(after_first.hosts[0].authority, addr.to_string());
    assert_eq!(after_first.hosts[0].route, "direct");
    assert_eq!(after_first.hosts[0].idle, 1);
    assert_eq!(after_first.hosts[0].active, 0);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    let after_second = client.pool_stats();
    assert_eq!(after_second.checkout_misses, 1);
    assert_eq!(after_second.checkout_hits, 1);
    assert_eq!(after_second.idle_pool_entries, 1);
    assert_eq!(after_second.checked_out_pool_handles, 0);
}

#[tokio::test]
async fn observer_fires_pool_hit_on_reused_connection() {
    let (addr, _counter) = h1_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .build()
        .unwrap();

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
    let (addr, _counter) = h1_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .build()
        .unwrap();

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
    let (addr, _counter) = h1_server().await;

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

        fn on_connection_event(&self, _event: &ConnectionEvent) {}
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(UriCapture(events_clone))
        .build()
        .unwrap();

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
    let (addr, _counter) = h1_server().await;

    // Client without observer — should not panic or error
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
