#![cfg(feature = "tokio")]

//! Integration tests targeting specific uncovered lines in:
//! - client/execute_send.rs (stale-if-error, digest retry, HSTS, finalize_response)
//! - client/execute_local.rs (mirrors execute_send)
//! - client/connection_lifecycle.rs (connection_protocol, fire_connection_metrics, checkin)
//! - client/dispatch_send.rs (stale retry, pool hit, H2 multiplex)

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

#[path = "execute_send_coverage/hsts_digest.rs"]
mod hsts_digest;

#[path = "execute_send_coverage/cache_stale.rs"]
mod cache_stale;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 6. Connection pool reuse (hit path in dispatch_send.rs:101-167)
//    Exercises the pool checkout hit path and checkin.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn connection_pool_reuse_exercises_hit_path() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: opens connection (pool miss)
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Second request: should reuse connection (pool hit)
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Third request: should also reuse
    let resp = client
        .get(&format!("http://{addr}/third"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Should have 3 requests but only 1 connection
    assert_eq!(
        counter.connections(),
        1,
        "should reuse the same connection for all 3 requests"
    );
    assert_eq!(counter.requests(), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7. no_connection_reuse forces new connections
//    Exercises the skip of pool checkout when no_connection_reuse is set.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn no_connection_reuse_opens_new_connection_each_time() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_connection_reuse()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Each request should open a new connection
    for _ in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    assert_eq!(
        counter.connections(),
        3,
        "no_connection_reuse should open a new connection each time"
    );
    assert_eq!(counter.requests(), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 8. H2 connection pool hit and multiplex
//    Exercises dispatch_send.rs with H2 connections (multiplex path).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn h2_connection_reuse_multiplexes() {
    let (addr, counter) = aioduct_test_server::h2::h2_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2-response"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Multiple sequential requests should all multiplex over the same connection
    for i in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/req{i}"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "h2-response");
    }

    // H2 multiplexes all requests over a single connection
    assert_eq!(
        counter.connections(),
        1,
        "H2 should multiplex all requests over one connection"
    );
    assert_eq!(counter.requests(), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 9. Rate limiter sleep path during execute
//    Exercises dispatch_send.rs:52-56 (rate limiter wait loop).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn rate_limiter_sleep_path_in_dispatch() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    // Set a very low rate limit so the second request must wait
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .rate_limiter(aioduct::RateLimiter::new(1, Duration::from_millis(100)))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let start = std::time::Instant::now();

    // First request: immediate
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Second request: must wait for rate limiter
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(90),
        "rate limiter should introduce delay, elapsed: {elapsed:?}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 12. Observer receives connection metrics on pool checkin
//     Exercises connection_lifecycle.rs:44-61 (fire_connection_metrics).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn observer_receives_connection_metrics() {
    use std::sync::Mutex;

    #[derive(Default, Clone)]
    struct MetricsObserver {
        conn_events: Arc<Mutex<Vec<String>>>,
    }

    impl aioduct::observer::RequestObserver for MetricsObserver {
        fn on_event(&self, _event: &aioduct::observer::RequestEvent) {}
        fn on_connection_event(&self, event: &aioduct::observer::ConnectionEvent) {
            let desc = format!("{:?}", event.phase);
            self.conn_events.lock().unwrap().push(desc);
        }
    }

    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("metrics-test")))
                .unwrap(),
        )
    })
    .await;

    let obs = MetricsObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let events = obs.conn_events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "observer should receive connection metrics events"
    );
    // Connection metrics should contain Metrics phase
    let has_metrics = events.iter().any(|e| e.contains("Metrics"));
    assert!(
        has_metrics,
        "connection events should include Metrics phase, got: {events:?}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 13. GET request with no body exercises None arm in execute
//     Exercises execute_send.rs:52-57 (None body → empty Full).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn get_request_with_no_body() {
    let (addr, _counter) = h1_server_with(|req| async move {
        use http_body_util::BodyExt;
        let method = req.method().to_string();
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "method={method} body_len={}",
            body_bytes.len()
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "should be GET request, got: {body}"
    );
    assert!(
        body.contains("body_len=0"),
        "GET request should have empty body, got: {body}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 14. Streaming body exercises the Streaming arm in execute
//     Exercises execute_send.rs:51 (Streaming body path).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn streaming_body_exercises_streaming_arm() {
    use http_body_util::BodyExt;

    let (addr, _counter) = h1_server_with(|req| async move {
        use http_body_util::BodyExt;
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "received={}",
            String::from_utf8_lossy(&body_bytes)
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Create a streaming body (not buffered)
    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("stream-payload"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("received=stream-payload"),
        "streaming body should be sent correctly, got: {body}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 17. Dispatch: stale connection retry path
//     Exercises dispatch_send.rs:169-213 (stale connection error → retry on fresh).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn stale_connection_retry_succeeds_on_fresh_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request_count = Arc::new(AtomicU32::new(0));
    let request_count2 = request_count.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = request_count2.clone();

            tokio::spawn(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);

                if n == 0 {
                    // First connection: serve one response with keep-alive,
                    // then RST on next request.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nfirst";
                    let _ = stream.write_all(response).await;
                    let _ = stream.flush().await;

                    // Wait for second request to arrive, then RST
                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    // RST the connection
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                } else {
                    // Subsequent connections: serve normally
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nretry!";
                    let _ = stream.write_all(response).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: establishes connection, gets pooled
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "first");

    // Second request: stale connection is detected and retried on fresh connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        "retry!",
        "stale connection should be transparently retried"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 18. Observer events during pool hit vs miss
//     Exercises dispatch_send.rs observer notifications for pool outcomes.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn observer_reports_pool_hit_and_miss() {
    use std::sync::Mutex;

    #[derive(Default, Clone)]
    struct PoolObserver {
        phases: Arc<Mutex<Vec<String>>>,
    }

    impl aioduct::observer::RequestObserver for PoolObserver {
        fn on_event(&self, event: &aioduct::observer::RequestEvent) {
            let name = match &event.phase {
                aioduct::observer::RequestPhase::PoolCheckoutComplete { outcome, .. } => {
                    format!("PoolCheckout:{outcome:?}")
                }
                _ => return,
            };
            self.phases.lock().unwrap().push(name);
        }
        fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
    }

    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let obs = PoolObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: pool miss
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    // Second request: pool hit
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let phases = obs.phases.lock().unwrap();
    let has_miss = phases.iter().any(|p| p.contains("Miss"));
    let has_hit = phases.iter().any(|p| p.contains("Hit"));
    assert!(
        has_miss,
        "first request should report pool Miss, got: {phases:?}"
    );
    assert!(
        has_hit,
        "second request should report pool Hit, got: {phases:?}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helper: install crypto provider for rustls tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(feature = "rustls")]
fn install_crypto() {
    aioduct_test_server::tls::install_crypto_provider();
}
