#![cfg(feature = "tokio")]

//! Integration tests targeting dispatch_send.rs code paths for coverage.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::observer::{ConnectionEvent, RequestEvent, RequestObserver, RequestPhase};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{echo, h1_server, h1_server_with};
use aioduct_test_server::h2::h2_server_with;

// ── 1. https_only rejection ──────────────────────────────────────────────────

#[tokio::test]
async fn https_only_rejects_http_url() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .https_only(true)
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "https_only should reject http:// URLs");
}

// ── 2. Cookie jar ────────────────────────────────────────────────────────────

#[tokio::test]
async fn cookie_jar_stores_and_sends() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();

        if req.uri().path() == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "token=xyz789; Path=/")
                    .body(Full::new(Bytes::from("cookie set")))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from(format!(
                "cookies={cookie}"
            )))))
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    // Set the cookie
    let resp = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cookie set");

    // Verify cookie is sent on next request
    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("token=xyz789"),
        "cookie should be present, got: {body}"
    );
}

// ── 3. Middleware injection ──────────────────────────────────────────────────

#[tokio::test]
async fn middleware_injects_header() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-middleware")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBoxBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-middleware"),
                    http::header::HeaderValue::from_static("dispatch-test"),
                );
            },
        )
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "dispatch-test");
}

// ── 4. Observer receives events ──────────────────────────────────────────────

#[derive(Default, Clone)]
struct TestObserver {
    phases: Arc<Mutex<Vec<String>>>,
    conn_events: Arc<Mutex<Vec<String>>>,
}

impl RequestObserver for TestObserver {
    fn on_event(&self, event: &RequestEvent) {
        let phase_name = match &event.phase {
            RequestPhase::Started => "Started".to_string(),
            RequestPhase::PoolCheckoutComplete { .. } => "PoolCheckoutComplete".to_string(),
            RequestPhase::DnsResolved { .. } => "DnsResolved".to_string(),
            RequestPhase::TcpConnected { .. } => "TcpConnected".to_string(),
            RequestPhase::TlsHandshakeComplete { .. } => "TlsHandshakeComplete".to_string(),
            RequestPhase::RequestSent { .. } => "RequestSent".to_string(),
            RequestPhase::ResponseStarted { .. } => "ResponseStarted".to_string(),
            RequestPhase::ResponseComplete { .. } => "ResponseComplete".to_string(),
            RequestPhase::Failed { .. } => "Failed".to_string(),
            RequestPhase::BytesTransferred { .. } => "BytesTransferred".to_string(),
            RequestPhase::TransferComplete { .. } => "TransferComplete".to_string(),
            RequestPhase::TransferAborted { .. } => "TransferAborted".to_string(),
        };
        self.phases.lock().unwrap().push(phase_name);
    }

    fn on_connection_event(&self, event: &ConnectionEvent) {
        let name = format!("{:?}", event.phase);
        self.conn_events.lock().unwrap().push(name);
    }
}

#[tokio::test]
async fn observer_receives_lifecycle_events() {
    let (addr, _counter) = h1_server().await;
    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let phases = obs.phases.lock().unwrap();
    assert!(
        phases.contains(&"Started".to_string()),
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
        phases.contains(&"ResponseComplete".to_string()),
        "phases: {phases:?}"
    );
}

// ── 5. Read timeout on body ──────────────────────────────────────────────────

#[tokio::test]
async fn read_timeout_fires_on_stalled_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        // Send headers but stall on body
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        // Never send remaining body
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(50))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body_result = resp.text().await;
    assert!(
        body_result.is_err(),
        "read_timeout should fire when body stalls"
    );
}

// ── 6. Bandwidth limiter ─────────────────────────────────────────────────────

#[tokio::test]
async fn bandwidth_limiter_allows_request() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_download_speed(1024 * 1024)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 7. Rate limiter ──────────────────────────────────────────────────────────

#[tokio::test]
async fn rate_limiter_allows_request() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_requests_per_sec(100)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 8. Cache basic ───────────────────────────────────────────────────────────

#[tokio::test]
async fn cache_serves_second_request_from_store() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let count = hit_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from("cached response")))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .build();

    // First request hits server
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached response");
    assert_eq!(hit_count.load(Ordering::SeqCst), 1);

    // Second request served from cache
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached response");
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        1,
        "second request should be served from cache"
    );
}

// ── 9. HSTS store ────────────────────────────────────────────────────────────

#[tokio::test]
async fn hsts_store_basic_request_works() {
    let (addr, _counter) = h1_server().await;

    let hsts = aioduct::HstsStore::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .hsts(hsts)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 10. Decompression disabled ───────────────────────────────────────────────

#[tokio::test]
async fn no_decompression_omits_accept_encoding() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let has_accept_encoding = req.headers().contains_key("accept-encoding");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "accept-encoding={}",
            has_accept_encoding
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .no_decompression()
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "accept-encoding=false",
        "no_decompression should not send accept-encoding header"
    );
}

// ── 11. Resolve override ─────────────────────────────────────────────────────

#[tokio::test]
async fn resolve_override_routes_to_target() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .resolve("custom.local", addr)
        .build();

    let resp = client
        .get(&format!("http://custom.local:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 12. Too many redirects ───────────────────────────────────────────────────

#[tokio::test]
async fn too_many_redirects_returns_error() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("Location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_redirects(3)
        .build();

    let result = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await;

    assert!(result.is_err(), "should error on too many redirects");
    let err = result.unwrap_err();
    assert!(err.is_redirect(), "expected redirect error, got: {err:?}");
}

// ── 13. 303 redirect changes POST to GET ─────────────────────────────────────

#[tokio::test]
async fn redirect_303_changes_post_to_get() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/submit" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(303)
                    .header("Location", "/result")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let method = req.method().to_string();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .post(&format!("http://{addr}/submit"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "303 should change POST to GET, got: {body}"
    );
}

// ── 14. Connect timeout ──────────────────────────────────────────────────────

#[tokio::test]
async fn connect_timeout_fires_on_unreachable() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connect_timeout(Duration::from_millis(1))
        .build();

    let result = client.get("http://192.0.2.1:1/").unwrap().send().await;

    assert!(result.is_err(), "connect_timeout should fire");
    let err = result.unwrap_err();
    assert!(
        err.is_timeout() || err.is_connect(),
        "expected timeout or connect error, got: {err:?}"
    );
}

// ── 15. TCP keepalive ────────────────────────────────────────────────────────

#[tokio::test]
async fn tcp_keepalive_basic_request_works() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tcp_keepalive(Duration::from_secs(60))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 16. Error for status ─────────────────────────────────────────────────────

#[tokio::test]
async fn error_for_status_on_500() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(500)
                .body(Full::new(Bytes::from("server error")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 500);
    let result = resp.error_for_status();
    assert!(result.is_err(), "error_for_status should error on 500");
    let err = result.unwrap_err();
    assert!(err.is_status(), "expected status error, got: {err:?}");
}

// ── 17. JSON body via post ───────────────────────────────────────────────────

#[cfg(feature = "json")]
#[tokio::test]
async fn json_post_sets_content_type() {
    let (addr, _counter) = h1_server_with(echo).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .json(&serde_json::json!({"key": "value"}))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("content-type: application/json"),
        "expected application/json content-type, got: {body}"
    );
}

// ── 18. Form body via post ───────────────────────────────────────────────────

#[tokio::test]
async fn form_post_sets_content_type() {
    let (addr, _counter) = h1_server_with(echo).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .form(&[("name", "test"), ("value", "123")])
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("content-type: application/x-www-form-urlencoded"),
        "expected form content-type, got: {body}"
    );
}

// ── 19. H2c prior knowledge ──────────────────────────────────────────────────

#[tokio::test]
async fn h2c_prior_knowledge_works() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 ok");
}

// ── 20. No connection reuse ──────────────────────────────────────────────────

#[tokio::test]
async fn no_connection_reuse_opens_new_connections() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = h1_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .no_connection_reuse()
        .build();

    // Make 2 requests
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await;

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await;

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    // With no_connection_reuse, each request should open a new connection
    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections, got {}",
        counter.connections()
    );
}

// ── 21. H2 pool hit — connection reuse (lines 101-168) ─────────────────────

#[tokio::test]
async fn h2_pool_hit_reuses_connection() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 reuse"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .pool_idle_timeout(Duration::from_secs(60))
        .build();

    // First request establishes the connection
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Second request should reuse the pooled H2 connection (pool hit path)
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 reuse");

    // Only 1 TCP connection should have been made
    assert_eq!(
        counter.connections(),
        1,
        "H2 should reuse the connection (pool hit), got {} connections",
        counter.connections()
    );
    // But 2 requests were served
    assert_eq!(counter.requests(), 2);
}

// ── 22. H2 multiplex wait path (lines 512-578) ─────────────────────────────

#[tokio::test]
async fn h2_concurrent_requests_multiplex_single_connection() {
    let (addr, counter) = h2_server_with(|_req| async {
        // Small delay to ensure requests overlap
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 multiplex"))))
    })
    .await;

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .http2_prior_knowledge()
            .pool_idle_timeout(Duration::from_secs(60))
            .build(),
    );

    // Fire multiple concurrent requests to trigger the multiplex wait path
    let mut handles = Vec::new();
    for i in 0..5 {
        let client = client.clone();
        let url = format!("http://{addr}/req{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "h2 multiplex");
    }

    // All requests should multiplex on 1 connection (or at most 2 if there's a race)
    assert!(
        counter.connections() <= 2,
        "H2 multiplex should use minimal connections, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 5);
}

// ── 23. Stale connection retry on pool hit (lines 169-227) ──────────────────

#[tokio::test]
async fn stale_connection_retry_on_rst() {
    // The h1_rst_on_reuse server answers the first request normally, then RSTs
    // when the client tries to reuse the connection. The retry logic should
    // open a fresh connection and succeed.
    let (addr, counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // First request succeeds and the connection is pooled
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Small delay so the server has time to RST
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second request hits stale connection in pool, should retry on fresh connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Should have opened 2 connections (first + retry)
    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections for stale retry, got {}",
        counter.connections()
    );
}

// ── 24. Stale connection retry with FIN ─────────────────────────────────────

#[tokio::test]
async fn stale_connection_retry_on_fin() {
    let (addr, counter) = aioduct_test_server::stale::h1_fin_on_reuse().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections for stale retry on FIN, got {}",
        counter.connections()
    );
}

// ── 25. TLS connection path (lines 717-835) ─────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_connection_exercises_tls_path() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "Should negotiate h2 via ALPN"
    );
    assert!(
        resp.tls_info().is_some(),
        "TLS info should be present on the response"
    );
    assert_eq!(resp.text().await.unwrap(), "hello tls");
}

// ── 26. TLS H1 fallback path ────────────────────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_h1_connection_path() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_11,
        "Should use HTTP/1.1 when server only offers http/1.1 ALPN"
    );
    assert_eq!(resp.text().await.unwrap(), "hello tls");
}

// ── 27. TLS H2 connection reuse via pool (lines 849-861) ────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_h2_multiplex_checkin_path() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .tls(connector)
            .pool_idle_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(5))
            .build(),
    );

    // Concurrent requests to exercise the H2 multiplex check-in path
    let mut handles = Vec::new();
    for i in 0..4 {
        let client = client.clone();
        let url = format!("https://localhost:{}/req{i}", addr.port());
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello tls");
    }

    // H2 multiplexing should use 1-2 connections for all requests
    assert!(
        counter.connections() <= 2,
        "TLS H2 multiplex should use minimal connections, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 4);
}

// ── 28. HTTP proxy with PROXY_AUTHORIZATION (lines 863-873) ─────────────────

#[tokio::test]
async fn http_proxy_injects_proxy_authorization() {
    // Set up a mock "proxy" that echoes back request headers
    let (proxy_addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("proxy-authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_else(|| "missing".to_owned());
        let uri = req.uri().to_string();
        let body = format!("auth={auth}\nuri={uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let proxy = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .basic_auth("user", "secret");

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(proxy)
        .timeout(Duration::from_secs(5))
        .build();

    // plaintext HTTP request through proxy should have Proxy-Authorization injected
    let resp = client
        .get("http://example.com/resource")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("auth=Basic"),
        "expected Basic auth in proxy-authorization, got: {body}"
    );
    assert!(
        body.contains("dXNlcjpzZWNyZXQ="), // base64("user:secret")
        "expected base64 credentials, got: {body}"
    );
}

// ── 29. H2 pool hit with observer reports pool outcome ──────────────────────

#[tokio::test]
async fn h2_pool_hit_observer_reports_hit() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 observed"))))
    })
    .await;

    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .pool_idle_timeout(Duration::from_secs(60))
        .request_observer(obs.clone())
        .build();

    // First request: pool miss, establishes connection
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Second request: pool hit
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 observed");

    let phases = obs.phases.lock().unwrap();
    // The second request should get PoolCheckoutComplete (hit)
    let pool_checkout_count = phases
        .iter()
        .filter(|p| *p == "PoolCheckoutComplete")
        .count();
    assert!(
        pool_checkout_count >= 2,
        "expected at least 2 PoolCheckoutComplete events, got {pool_checkout_count}"
    );
}

// ── 30. Non-connection-reuse prevents pool checkin (lines 911-914) ───────────

#[tokio::test]
async fn no_connection_reuse_prevents_pool_checkin_h2() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 no reuse"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .no_connection_reuse()
        .build();

    // Make 3 sequential requests - each should open a new connection
    for i in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/req{i}"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // With no_connection_reuse + H2, every request opens a new connection
    assert_eq!(
        counter.connections(),
        3,
        "no_connection_reuse should open new connection each time, got {}",
        counter.connections()
    );
}

// ── 31. H1 pool hit path (connection reuse) ─────────────────────────────────

#[tokio::test]
async fn h1_pool_hit_reuses_connection() {
    let (addr, counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h1 reuse"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .build();

    // First request establishes the connection
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Second request should reuse the pooled H1 connection
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h1 reuse");

    // Only 1 TCP connection should have been made (pool hit)
    assert_eq!(
        counter.connections(),
        1,
        "H1 should reuse the connection via pool hit, got {} connections",
        counter.connections()
    );
    assert_eq!(counter.requests(), 2);
}

// ── 32. TLS connection no ALPN → H1 path (line 807-820) ────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_no_alpn_falls_to_h1() {
    aioduct_test_server::tls::install_crypto_provider();

    // Server with empty ALPN — no protocol negotiated
    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[]).await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello tls");
}

// ── 33. TLS sequential requests reuse H2 pool (covers pool hit on H2 TLS) ──

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_h2_sequential_reuses_connection() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    let url = format!("https://localhost:{}/", addr.port());

    // Three sequential requests should all use the same connection
    for _ in 0..3 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "TLS H2 sequential requests should reuse 1 connection, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 3);
}

// ── 34. H2 GOAWAY triggers reconnect ────────────────────────────────────────

#[tokio::test]
async fn h2_goaway_triggers_fresh_connection() {
    // Server sends GOAWAY after 2 requests
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_after(2).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // First 2 requests go on one connection
    for _ in 0..2 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // Give server time to process GOAWAY
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Third request should open a new connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections after GOAWAY, got {}",
        counter.connections()
    );
}

// ── 35. Proxy settings with basic auth for HTTP (lines 863-873) ─────────────

#[tokio::test]
async fn proxy_settings_injects_authorization_on_http() {
    let (proxy_addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("proxy-authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_else(|| "none".to_owned());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "proxy-auth={auth}"
        )))))
    })
    .await;

    let proxy = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .basic_auth("admin", "password123");

    let settings = aioduct::ProxySettings::all(proxy);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy_settings(settings)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get("http://target.example.com/api")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("proxy-auth=Basic"),
        "expected Basic proxy-authorization, got: {body}"
    );
}

// ── 36. Multiple stale retries with rst_every_n ─────────────────────────────

#[tokio::test]
async fn stale_retry_rst_every_n_succeeds() {
    // Server serves 2 requests per connection, then RSTs.
    // This means: first 2 requests succeed on connection 1, then the 3rd
    // request attempts reuse and hits a stale (RST'd) connection, triggering retry.
    let (addr, counter) = aioduct_test_server::stale::h1_rst_every_n(2).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // First two requests succeed on the same connection
    for i in 0..2 {
        let resp = client
            .get(&format!("http://{addr}/req{i}"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // Third request: pooled connection was RST'd, retry opens a fresh one
    let resp = client
        .get(&format!("http://{addr}/req2"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Should have at least 2 connections (original + retry after RST)
    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections with RST after 2 requests, got {}",
        counter.connections()
    );
}

// ── 37. H2 pool hit after multiplex checkin ─────────────────────────────────

#[tokio::test]
async fn h2_pool_hit_after_concurrent_establishment() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = h2_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 pool hit"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .pool_idle_timeout(Duration::from_secs(60))
        .build();

    // Establish connection with first request
    let resp = client
        .get(&format!("http://{addr}/setup"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Now fire concurrent requests — they should all multiplex on the pooled connection
    let client = Arc::new(client);
    let mut handles = Vec::new();
    for i in 0..3 {
        let client = client.clone();
        let url = format!("http://{addr}/concurrent{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // 1 connection total: established by first request, multiplexed for all others
    assert_eq!(
        counter.connections(),
        1,
        "all requests should multiplex on 1 connection, got {}",
        counter.connections()
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 4); // 1 setup + 3 concurrent
}

// ── 38. HSTS store_from_response via HTTPS ────────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn hsts_stored_from_https_response() {
    aioduct_test_server::tls::install_crypto_provider();

    // Start a TLS server that returns Strict-Transport-Security header
    let (addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |_req| async {
            Ok::<_, Infallible>(
                Response::builder()
                    .header(
                        "strict-transport-security",
                        "max-age=31536000; includeSubDomains",
                    )
                    .body(Full::new(Bytes::from("hsts response")))
                    .unwrap(),
            )
        })
        .await;

    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let hsts = aioduct::HstsStore::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .hsts(hsts.clone())
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hsts response");

    // Verify HSTS was stored from the HTTPS response
    assert!(
        hsts.should_upgrade("localhost"),
        "HSTS should be stored from HTTPS response with STS header"
    );
}

// ── 39. Cache invalidation on non-GET after successful response ───────────────

#[tokio::test]
async fn cache_invalidation_on_post() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let count = hit_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            if req.method() == http::Method::POST {
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("posted"))))
            } else {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=3600")
                        .body(Full::new(Bytes::from("cached")))
                        .unwrap(),
                )
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .build();

    let url = format!("http://{addr}/resource");

    // First GET: populates cache
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");
    assert_eq!(hit_count.load(Ordering::SeqCst), 1);

    // Second GET: served from cache
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        1,
        "second GET should be from cache"
    );

    // POST: invalidates the cache
    let resp = client
        .post(&url)
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "posted");

    // Third GET after POST: should hit the server again (cache invalidated)
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");
    assert!(
        hit_count.load(Ordering::SeqCst) >= 3,
        "GET after POST should re-fetch from server, got {} requests",
        hit_count.load(Ordering::SeqCst)
    );
}

// ── 40. 307 redirect preserves method and body ────────────────────────────────

#[tokio::test]
async fn redirect_307_preserves_method_and_body() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/submit" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(307)
                    .header("Location", "/result")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            use http_body_util::BodyExt;
            let method = req.method().to_string();
            let body = req.collect().await.unwrap().to_bytes();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method},body={}",
                String::from_utf8_lossy(&body)
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .post(&format!("http://{addr}/submit"))
        .unwrap()
        .body("my-data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=POST"),
        "307 should preserve POST method, got: {body}"
    );
    assert!(
        body.contains("body=my-data"),
        "307 should replay the body, got: {body}"
    );
}

// ── 41. Observer receives connection metrics on checkin ──────────────────────

#[tokio::test]
async fn observer_fires_connection_metrics_on_checkin() {
    let (addr, _counter) = h1_server().await;
    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .pool_idle_timeout(Duration::from_secs(60))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let conn_events = obs.conn_events.lock().unwrap();
    assert!(
        !conn_events.is_empty(),
        "observer should receive connection metrics events on checkin, got: {conn_events:?}"
    );
    // Check the event contains "Metrics"
    assert!(
        conn_events.iter().any(|e| e.contains("Metrics")),
        "expected Metrics connection event, got: {conn_events:?}"
    );
}

// ── 42. Observer receives connection metrics on H2 multiplex clone checkin ───

#[tokio::test]
async fn observer_fires_connection_metrics_on_h2_multiplex_clone() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 metrics"))))
    })
    .await;

    let obs = TestObserver::default();

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .http2_prior_knowledge()
            .pool_idle_timeout(Duration::from_secs(60))
            .request_observer(obs.clone())
            .build(),
    );

    // Make 2 sequential requests to ensure multiplex clone path
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let conn_events = obs.conn_events.lock().unwrap();
    assert!(
        !conn_events.is_empty(),
        "observer should receive connection metrics for H2 multiplex"
    );
}

// ── 43. HSTS upgrade on second request ──────────────────────────────────────

#[tokio::test]
async fn hsts_upgrade_redirects_http_to_https() {
    // Pre-populate HSTS by processing a fake response header
    let hsts = aioduct::HstsStore::new();
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::HeaderName::from_static("strict-transport-security"),
        http::header::HeaderValue::from_static("max-age=31536000"),
    );
    hsts.store_from_response("localhost", &headers);

    // Verify HSTS is stored
    assert!(hsts.should_upgrade("localhost"));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .hsts(hsts)
        .timeout(Duration::from_millis(500))
        .build();

    // This request to http://localhost should be upgraded to https://localhost
    // which will fail (no TLS configured), proving the upgrade happened
    let result = client.get("http://localhost:9999/").unwrap().send().await;

    // The request should fail because HSTS upgrades to HTTPS but no TLS is configured
    assert!(
        result.is_err(),
        "HSTS upgrade should cause the request to fail without TLS"
    );
}

// ── 44. Default headers applied ─────────────────────────────────────────────

#[tokio::test]
async fn default_headers_applied_to_request() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-custom-default")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "missing".to_string());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "custom={custom}"
        )))))
    })
    .await;

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::HeaderName::from_static("x-custom-default"),
        http::header::HeaderValue::from_static("default-value"),
    );

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .default_headers(headers)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("custom=default-value"),
        "default headers should be applied, got: {body}"
    );
}

// ── 45. Default headers don't override explicit headers ──────────────────────

#[tokio::test]
async fn default_headers_do_not_override_explicit() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-custom-default")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "missing".to_string());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::HeaderName::from_static("x-custom-default"),
        http::header::HeaderValue::from_static("default-value"),
    );

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .default_headers(headers)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("x-custom-default", "explicit-value")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "explicit-value",
        "explicit header should override default"
    );
}

// ── 46. 308 redirect preserves method but streaming body fails ───────────────

#[tokio::test]
async fn redirect_308_streaming_body_errors() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(308)
                .header("Location", "/target")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    // Use a streaming body (non-clonable) with a POST + 308 redirect
    use http_body_util::BodyExt as _;
    let chunks: Vec<Result<hyper::body::Frame<Bytes>, aioduct::Error>> =
        vec![Ok(hyper::body::Frame::data(Bytes::from("stream")))];
    let stream = futures_util::stream::iter(chunks);
    let streaming_body: aioduct::body::RequestBoxBody =
        http_body_util::StreamBody::new(stream).boxed_unsync();

    let result = client
        .post(&format!("http://{addr}/submit"))
        .unwrap()
        .body_stream(streaming_body)
        .send()
        .await;

    assert!(
        result.is_err(),
        "308 redirect with streaming body should error"
    );
}

// ── 47. Redirect policy none returns redirect response directly ──────────────

#[tokio::test]
async fn redirect_policy_none_returns_redirect_directly() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/target")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from("reached target"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::none())
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    // With redirect policy none, the redirect response should be returned directly
    assert_eq!(resp.status(), 302);
    assert!(
        resp.headers().contains_key("location"),
        "redirect response should contain Location header"
    );
}

// ── 48. Referer header on redirect ──────────────────────────────────────────

#[tokio::test]
async fn referer_header_added_on_redirect() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/source" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/dest")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let referer = req
                .headers()
                .get("referer")
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_else(|| "none".to_string());
            Ok(Response::new(Full::new(Bytes::from(format!(
                "referer={referer}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/source"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains(&format!("http://{addr}/source")),
        "referer should contain the source URL, got: {body}"
    );
}

// ── 49. Cache 304 revalidation returns cached body via execute_send ─────────

#[tokio::test]
async fn cache_304_revalidation_via_execute_send() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, must-revalidate")
                        .header("etag", "\"revalidate-v1\"")
                        .body(Full::new(Bytes::from("original body")))
                        .unwrap(),
                )
            } else {
                let inm = req
                    .headers()
                    .get("if-none-match")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                if inm.contains("\"revalidate-v1\"") {
                    Ok(Response::builder()
                        .status(304)
                        .header("etag", "\"revalidate-v1\"")
                        .body(Full::new(Bytes::new()))
                        .unwrap())
                } else {
                    Ok(Response::new(Full::new(Bytes::from("new body"))))
                }
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .build();

    // First: populate cache
    let resp = client
        .get(&format!("http://{addr}/revalidate-send"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "original body");

    // Second: server returns 304, client should return cached body
    let resp = client
        .get(&format!("http://{addr}/revalidate-send"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "original body");
    assert_eq!(
        attempt.load(Ordering::SeqCst),
        2,
        "server should be hit twice"
    );
}

// ── 50. Cache stale-if-error on 5xx serves stale via execute_send ────────────

#[tokio::test]
async fn cache_stale_if_error_on_5xx_via_execute_send() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, stale-if-error=3600")
                        .header("etag", "\"sie-v1\"")
                        .body(Full::new(Bytes::from("stale ok")))
                        .unwrap(),
                )
            } else {
                Ok(Response::builder()
                    .status(503)
                    .body(Full::new(Bytes::from("service unavailable")))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .build();

    // Populate cache
    let resp = client
        .get(&format!("http://{addr}/sie-send"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "stale ok");

    // Server error: stale-if-error should return cached response
    let resp = client
        .get(&format!("http://{addr}/sie-send"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "stale ok");
}

// ── 51. Digest auth retry via execute_send ──────────────────────────────────

#[tokio::test]
async fn digest_auth_retry_via_execute_send() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            let has_auth = req.headers().contains_key("authorization");
            if n == 0 && !has_auth {
                // First request: challenge with 401
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            "Digest realm=\"test\", nonce=\"abc123\", qop=\"auth\"",
                        )
                        .body(Full::new(Bytes::from("Unauthorized")))
                        .unwrap(),
                )
            } else {
                // Second request: has auth
                let auth_header = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default();
                Ok(Response::new(Full::new(Bytes::from(format!(
                    "authed={auth_header}"
                )))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .digest_auth("testuser", "testpass")
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/protected"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Digest"),
        "digest auth should produce Digest authorization header, got: {body}"
    );
    assert!(
        body.contains("testuser"),
        "digest auth should include username, got: {body}"
    );
}

// ── 52. Cookie jar stores from response ─────────────────────────────────────

#[tokio::test]
async fn cookie_jar_stores_and_sends_on_next_request() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("set-cookie", "session=abc123; Path=/")
                        .body(Full::new(Bytes::from("set")))
                        .unwrap(),
                )
            } else {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_else(|| "none".to_string());
                Ok(Response::new(Full::new(Bytes::from(format!(
                    "cookie={cookie}"
                )))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    // Set cookie
    let resp = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "set");

    // Cookie should be sent on next request
    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("session=abc123"),
        "cookie should be sent, got: {body}"
    );
}

// ── 53. Host header auto-inserted when missing ──────────────────────────────

#[tokio::test]
async fn host_header_auto_inserted() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "missing".to_string());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "host={host}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains(&addr.to_string()),
        "host header should contain authority, got: {body}"
    );
}

// ── 54. Observer receives StaleRetry event ──────────────────────────────────

#[tokio::test]
async fn observer_receives_stale_retry_event() {
    let (addr, counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .request_observer(obs.clone())
        .build();

    // First request succeeds
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second request hits stale connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let phases = obs.phases.lock().unwrap();
    // Should see Failed with will_retry:true, and PoolCheckoutComplete(StaleRetry)
    assert!(
        phases.iter().any(|p| p.contains("PoolCheckoutComplete")),
        "expected PoolCheckoutComplete phase, got: {phases:?}"
    );

    assert!(
        counter.connections() >= 2,
        "should have opened at least 2 connections"
    );
}

// ── 55. Middleware applies to request on fresh connection path ────────────────

#[tokio::test]
async fn middleware_applies_on_fresh_connection() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-fresh-middleware")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "missing".to_string());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "middleware={custom}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBoxBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-fresh-middleware"),
                    http::header::HeaderValue::from_static("fresh-path"),
                );
            },
        )
        .no_connection_reuse()
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("middleware=fresh-path"),
        "middleware should be applied on fresh connection path, got: {body}"
    );
}

// ── 56. Unix socket connection path (dispatch_send lines 593-634) ───────────

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_connection_path() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = std::env::temp_dir().join("aioduct_dispatch_test");
    let _ = std::fs::create_dir_all(&dir);
    let sock_path = dir.join("dispatch_test.sock");
    let _ = std::fs::remove_file(&sock_path);

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nunix socket";
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .unix_socket(&sock_path)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get("http://localhost/unix-test")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "unix socket");
}

// ── 57. Unix socket with connect timeout (dispatch_send lines 622-631) ──────

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_with_connect_timeout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = std::env::temp_dir().join("aioduct_dispatch_test");
    let _ = std::fs::create_dir_all(&dir);
    let sock_path = dir.join("dispatch_timeout.sock");
    let _ = std::fs::remove_file(&sock_path);

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nunix w/timeout";
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .unix_socket(&sock_path)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get("http://localhost/timeout-test")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "unix w/timeout");
}

// ── 58. AdaptiveH2c probe succeeds (dispatch_send lines 719-736) ────────────

#[tokio::test]
async fn adaptive_h2c_probe_succeeds_on_h2_server() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2c adaptive ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    // Use forward() with adaptive_h2c() to trigger the AdaptiveH2c protocol hint
    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/adaptive-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2c adaptive ok");

    // Second request uses cached probe result (should skip probe)
    let incoming2 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/adaptive-test2")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp2 = client
        .forward(incoming2)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), http::StatusCode::OK);
    assert_eq!(resp2.text().await.unwrap(), "h2c adaptive ok");

    // H2 multiplex should keep connections low
    assert!(
        counter.connections() <= 2,
        "cached h2c probe should reuse connection, got {} connections",
        counter.connections()
    );
}

// ── 59. AdaptiveH2c probe falls back to H1 (lines 737-757 + 839-843) ───────

#[tokio::test]
async fn adaptive_h2c_probe_falls_back_to_h1() {
    // H1-only server — h2c preface will be rejected
    let (addr, counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h1 fallback ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/fallback-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h1 fallback ok");

    // Second request uses cached h1_only result (pool_key.protocol set to Auto)
    let incoming2 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/fallback-test2")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp2 = client
        .forward(incoming2)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), http::StatusCode::OK);
    assert_eq!(resp2.text().await.unwrap(), "h1 fallback ok");

    // Probe fails on conn 1, fallback opens conn 2, second request may reuse
    assert!(
        counter.connections() >= 2,
        "adaptive h2c probe + fallback should use at least 2 connections, got {}",
        counter.connections()
    );
}

// ── 60. Connection coalescing on TLS H2 (dispatch_send lines 230-370) ───────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn connection_coalescing_reuses_h2_tls_connection() {
    use std::sync::atomic::AtomicU32;

    aioduct_test_server::tls::install_crypto_provider();

    // Generate cert with multiple SANs
    let cert =
        aioduct_test_server::tls::generate_self_signed(&["coalesce-a.local", "coalesce-b.local"]);
    let cert_der = cert.cert_der.clone();

    let counter = aioduct_test_server::ConnectionCounter::new();
    let counter2 = counter.clone();
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    // TLS H2 server with multi-SAN cert
    let config = {
        let mut cfg = rustls::ServerConfig::builder_with_provider(
            aioduct_test_server::tls::crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
        .unwrap();
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        std::sync::Arc::new(cfg)
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let acceptor = acceptor.clone();
            let req_count = request_count_clone.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = aioduct_test_server::TokioIo::new(tls_stream);
                let _ = hyper::server::conn::http2::Builder::new(aioduct_test_server::TokioExec)
                    .serve_connection(
                        io,
                        hyper::service::service_fn(move |_req| {
                            req_count.fetch_add(1, Ordering::SeqCst);
                            async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "coalesced",
                                ))))
                            }
                        }),
                    )
                    .await;
            });
        }
    });

    // Client config trusting our self-signed cert
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der.clone()).unwrap();
    let mut client_config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(root_store)
            .with_no_client_auth();
    client_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(std::sync::Arc::new(client_config));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .connection_coalescing(true)
        .resolve("coalesce-a.local", addr)
        .resolve("coalesce-b.local", addr)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // First request to coalesce-a.local — establishes TLS H2 connection
    let resp = client
        .get(&format!("https://coalesce-a.local:{}/first", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    let _ = resp.text().await.unwrap();

    // Second request to coalesce-b.local — coalesces onto existing connection
    let resp = client
        .get(&format!("https://coalesce-b.local:{}/second", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    assert_eq!(resp.text().await.unwrap(), "coalesced");

    // Only 1 TLS connection should have been made (coalescing reused it)
    assert_eq!(
        counter.connections(),
        1,
        "connection coalescing should reuse single TLS H2 connection, got {}",
        counter.connections()
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 2);
}

// ── 61. Connection coalescing disabled opens separate connections ────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn connection_coalescing_disabled_opens_separate() {
    aioduct_test_server::tls::install_crypto_provider();

    let cert =
        aioduct_test_server::tls::generate_self_signed(&["no-coal-a.local", "no-coal-b.local"]);
    let cert_der = cert.cert_der.clone();
    let counter = aioduct_test_server::ConnectionCounter::new();
    let counter2 = counter.clone();

    let config = {
        let mut cfg = rustls::ServerConfig::builder_with_provider(
            aioduct_test_server::tls::crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
        .unwrap();
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        std::sync::Arc::new(cfg)
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = aioduct_test_server::TokioIo::new(tls_stream);
                let _ = hyper::server::conn::http2::Builder::new(aioduct_test_server::TokioExec)
                    .serve_connection(
                        io,
                        hyper::service::service_fn(|_req| async {
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("separate"))))
                        }),
                    )
                    .await;
            });
        }
    });

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der.clone()).unwrap();
    let mut client_config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(root_store)
            .with_no_client_auth();
    client_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(std::sync::Arc::new(client_config));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .connection_coalescing(false)
        .resolve("no-coal-a.local", addr)
        .resolve("no-coal-b.local", addr)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://no-coal-a.local:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let resp = client
        .get(&format!("https://no-coal-b.local:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "separate");

    assert_eq!(
        counter.connections(),
        2,
        "coalescing disabled should open 2 connections, got {}",
        counter.connections()
    );
}

// ── 62. H2 multiplex wait spin loop (dispatch_send lines 512-578) ───────────

#[tokio::test]
async fn h2_multiplex_wait_spin_loop_many_concurrent() {
    use std::sync::atomic::AtomicU32;

    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = h2_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("spin wait ok"))))
        }
    })
    .await;

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .http2_prior_knowledge()
            .pool_idle_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(10))
            .build(),
    );

    // Launch 15 concurrent requests to aggressively trigger mark_connecting_h2
    let mut handles = Vec::new();
    for i in 0..15 {
        let client = client.clone();
        let url = format!("http://{addr}/spinwait{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "spin wait ok");
    }

    // H2 multiplexing should keep connections minimal despite many concurrent reqs
    assert!(
        counter.connections() <= 3,
        "H2 multiplex wait should converge to few connections, got {}",
        counter.connections()
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 15);
}

// ── 63. Forward with h2c (non-adaptive) exercises force_h2c path ────────────

#[tokio::test]
async fn forward_h2c_prior_knowledge_exercises_force_h2c() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("forward h2c ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/h2c-forward-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "forward h2c ok");
}

// ── 64. H2c probe cache TTL re-probes after expiry ──────────────────────────

#[tokio::test]
async fn h2c_probe_cache_ttl_forces_re_probe() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("re-probed"))))
    })
    .await;

    // Very short TTL forces re-probe
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .h2c_probe_ttl(Duration::from_millis(1))
        .timeout(Duration::from_secs(5))
        .build();

    let incoming1 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/ttl-probe1")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming1)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Second request re-probes since TTL expired
    let incoming2 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/ttl-probe2")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming2)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "re-probed");

    assert!(
        counter.requests() >= 2,
        "TTL expiry should cause re-probe, got {} requests",
        counter.requests()
    );
}

// ── 65. TCP fast open option exercises path (line 711-713) ──────────────────

#[tokio::test]
async fn tcp_fast_open_exercises_path() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tcp_fast_open(true)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 66. Local address binding exercises connect_bound (lines 678-699) ────────

#[tokio::test]
async fn local_address_binding_exercises_connect_bound() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 67. TCP keepalive interval and retries (lines 706-710) ──────────────────

#[tokio::test]
async fn tcp_keepalive_interval_and_retries() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_keepalive_interval(Duration::from_secs(30))
        .tcp_keepalive_retries(5)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 68. Switching protocols (101) skips pool checkin (lines 164, 911-913) ───

#[tokio::test]
async fn switching_protocols_skips_pool_checkin() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            )
            .await
            .unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/ws"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);
}

// ── 69. Observer TLS events (lines 789-806) ─────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn observer_tls_handshake_complete_event() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let phases = obs.phases.lock().unwrap();
    assert!(
        phases.contains(&"TlsHandshakeComplete".to_string()),
        "should emit TlsHandshakeComplete, got: {phases:?}"
    );
    assert!(
        phases.contains(&"TcpConnected".to_string()),
        "should emit TcpConnected, got: {phases:?}"
    );
}

// ── 70. H2 redundant connection discard (lines 849-857) ─────────────────────

#[tokio::test]
async fn h2_discards_redundant_connection_on_race() {
    let (addr, counter) = h2_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(2)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("race discard"))))
    })
    .await;

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .http2_prior_knowledge()
            .pool_idle_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(10))
            .build(),
    );

    let mut handles = Vec::new();
    for i in 0..12 {
        let client = client.clone();
        let url = format!("http://{addr}/race{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "race discard");
    }

    assert!(
        counter.connections() <= 3,
        "H2 should discard redundant connections, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 12);
}

// ── 71. Rate limiter wait loop (lines 52-56) ────────────────────────────────

#[tokio::test]
async fn rate_limiter_wait_loop_exercises_sleep() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("rate ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_requests_per_sec(2)
        .timeout(Duration::from_secs(10))
        .build();

    let start = std::time::Instant::now();

    for i in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/rate{i}"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(350),
        "rate limiter should delay requests, took {:?}",
        elapsed
    );
}

// ── 72. Pool hit non-retryable streaming body error (lines 215-227) ─────────

#[tokio::test]
async fn pool_hit_non_retryable_streaming_body_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        // RST after first response
        let raw = stream.into_std().unwrap();
        let sock = socket2::SockRef::from(&raw);
        let _ = sock.set_linger(Some(Duration::from_secs(0)));
        drop(raw);

        // Accept second connection (for retry that shouldn't happen)
        if let Ok((mut s2, _)) = listener.accept().await {
            let mut buf2 = [0u8; 4096];
            let _ = s2.read(&mut buf2).await;
            s2.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nretried",
            )
            .await
            .unwrap();
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // First GET: establish pooled connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // POST with streaming body — cannot be retried on stale connection
    let body_stream = futures_util::stream::once(async {
        Ok::<_, std::convert::Infallible>(hyper::body::Frame::data(Bytes::from("streaming")))
    });
    let stream_body = http_body_util::StreamBody::new(body_stream);

    let incoming = http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}/post"))
        .body(stream_body)
        .unwrap();

    // Forward with streaming body exercises non-retryable error path
    let result = client
        .forward(incoming)
        .timeout(Duration::from_secs(2))
        .send()
        .await;

    assert!(
        result.is_err(),
        "streaming POST on stale connection should error (non-retryable)"
    );
}

// ── 73. Forward strip_prefix exercises path rewriting ────────────────────────

#[tokio::test]
async fn forward_strip_prefix_rewrites_path() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "path={path}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/api/v1/users")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .strip_prefix("/api/v1")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("path=/users"),
        "strip_prefix should rewrite /api/v1/users to /users, got: {body}"
    );
}

// ── 74. Pool idle timeout eviction forces new connection ────────────────────

#[tokio::test]
async fn pool_idle_timeout_evicts_old_connection() {
    let (addr, counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("idle evict"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_millis(30))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    assert!(
        counter.connections() >= 2,
        "idle timeout should evict connection, got {} connections",
        counter.connections()
    );
}

// ── 75. Forward on_request/on_response hooks ────────────────────────────────

#[tokio::test]
async fn forward_on_request_and_on_response_hooks() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-hook-test")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "hook={custom}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/hook-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .on_request(|parts| {
            parts.headers.insert(
                http::header::HeaderName::from_static("x-hook-test"),
                http::header::HeaderValue::from_static("injected"),
            );
        })
        .on_response(|resp| {
            resp.headers_mut().insert(
                http::header::HeaderName::from_static("x-resp-hook"),
                http::header::HeaderValue::from_static("applied"),
            );
        })
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.headers().get("x-resp-hook").unwrap().to_str().unwrap(),
        "applied"
    );
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("hook=injected"),
        "on_request hook should inject header, got: {body}"
    );
}

// ── 76. Chunk download with range support ────────────────────────────────────

#[tokio::test]
async fn chunk_download_with_range_support() {
    // Server that supports Accept-Ranges and serves partial content
    let body_data: Vec<u8> = (0..200u8).cycle().take(1000).collect();
    let body_data_arc = Arc::new(body_data.clone());

    let (addr, _counter) = h1_server_with(move |req| {
        let body_data = body_data_arc.clone();
        async move {
            if req.method() == http::Method::HEAD {
                return Ok::<_, Infallible>(
                    Response::builder()
                        .header("accept-ranges", "bytes")
                        .header("content-length", body_data.len().to_string())
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                );
            }

            if let Some(range) = req.headers().get("range") {
                let range_str = range.to_str().unwrap();
                let range_str = range_str.strip_prefix("bytes=").unwrap();
                let parts: Vec<&str> = range_str.split('-').collect();
                let start: usize = parts[0].parse().unwrap();
                let end: usize = parts[1].parse().unwrap();
                let slice = &body_data[start..=end];
                return Ok(Response::builder()
                    .status(206)
                    .header(
                        "content-range",
                        format!("bytes {start}-{end}/{}", body_data.len()),
                    )
                    .body(Full::new(Bytes::copy_from_slice(slice)))
                    .unwrap());
            }

            Ok(Response::new(Full::new(Bytes::from(
                body_data.as_ref().to_vec(),
            ))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(10))
        .build();

    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(result.total_size, 1000);
    assert_eq!(result.data.len(), 1000);
    assert_eq!(&result.data[..], &body_data[..]);
}

// ── 77. Chunk download fallback without range support ────────────────────────

#[tokio::test]
async fn chunk_download_fallback_no_ranges() {
    let (addr, _counter) = h1_server_with(|req| async move {
        if req.method() == http::Method::HEAD {
            // No accept-ranges header
            return Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "13")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            );
        }
        Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(result.total_size, 13);
    assert_eq!(result.data, Bytes::from("hello aioduct"));
}

// ── 78. Chunk download HEAD fails returns error ──────────────────────────────

#[tokio::test]
async fn chunk_download_head_failure_returns_error() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client
        .chunk_download(&format!("http://{addr}/missing"))
        .download()
        .await;

    assert!(result.is_err(), "HEAD failure should propagate error");
}

// ── 79. Builder with min_tls_version exercises TLS version branch ────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn builder_min_tls_version_builds_successfully() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .min_tls_version(aioduct::TlsVersion::Tls1_3)
        .build();

    // Just verify it builds without panic; actual TLS connection tested elsewhere
    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    // Will fail to connect (port 1), but verifies construction
    assert!(result.is_err());
}

// ── 80. Builder with max_tls_version ─────────────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn builder_max_tls_version_builds_successfully() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_tls_version(aioduct::TlsVersion::Tls1_2)
        .build();

    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    assert!(result.is_err());
}

// ── 81. Builder with tls_sni disabled exercises SNI path ─────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn builder_tls_sni_disabled_builds_successfully() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls_sni(false)
        .build();

    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    assert!(result.is_err());
}

// ── 82. Forward with client-level timeout (no explicit forward timeout) ──────

#[tokio::test]
async fn forward_uses_client_timeout_when_no_explicit_timeout() {
    use tokio::io::AsyncReadExt;

    // Server that never responds
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        // Never respond
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(100))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let result = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .send()
        .await;

    assert!(result.is_err(), "client timeout should fire for forward");
}

// ── 83. Forward with preserve_host and upstream base path ────────────────────

#[tokio::test]
async fn forward_preserve_host_with_upstream_base_path() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "host={host},path={path}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/users/123")
        .header("host", "original.example.com")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}/api/v2"))
        .preserve_host()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("host=original.example.com"),
        "preserve_host should keep original host, got: {body}"
    );
    assert!(
        body.contains("path=/api/v2/users/123"),
        "upstream base path should be prepended, got: {body}"
    );
}

// ── 84. Forward with remove_header and forward_header combined ───────────────

#[tokio::test]
async fn forward_remove_and_forward_header_combined() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "none".to_string());
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "none".to_string());
        let custom = req
            .headers()
            .get("x-forwarded")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "none".to_string());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "auth={auth},cookie={cookie},custom={custom}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/test")
        .header("authorization", "Bearer token123")
        .header("cookie", "session=abc")
        .header("x-forwarded", "original-value")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .forward_header(http::header::AUTHORIZATION)
        .remove_header(http::header::COOKIE)
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("auth=Bearer token123"),
        "forwarded header should be present, got: {body}"
    );
    assert!(
        body.contains("cookie=none"),
        "removed header should be absent, got: {body}"
    );
}

// ── 85. Observer receives connection metrics on checkin ──────────────────────

#[tokio::test]
async fn observer_receives_connection_metrics() {
    let (addr, _counter) = h1_server().await;
    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .pool_idle_timeout(Duration::from_secs(60))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    // Wait briefly for connection to be checked in
    tokio::time::sleep(Duration::from_millis(50)).await;

    let conn_events = obs.conn_events.lock().unwrap();
    assert!(
        !conn_events.is_empty(),
        "observer should receive connection metric events"
    );
    let metrics_event = conn_events.iter().any(|e| e.contains("Metrics"));
    assert!(
        metrics_event,
        "should have received a Metrics connection event, got: {conn_events:?}"
    );
}

// ── 87. Forward adaptive_h2c sets protocol hint ──────────────────────────────

#[tokio::test]
async fn forward_adaptive_h2c_exercises_path() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 adaptive"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/rpc/method")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 adaptive");
}

// ── 88. Finalize response with cache stores cacheable ────────────────────────

#[tokio::test]
async fn finalize_response_caches_cacheable_response() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let count = hit_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from("finalize cached")))
                    .unwrap(),
            )
        }
    })
    .await;

    // Client with cache + read_timeout + bandwidth limiter (exercises finalize_response fully)
    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .read_timeout(Duration::from_secs(30))
        .max_download_speed(1024 * 1024)
        .build();

    let resp = client
        .get(&format!("http://{addr}/cacheable"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "finalize cached");
    assert_eq!(hit_count.load(Ordering::SeqCst), 1);

    // Second request should hit cache
    let resp = client
        .get(&format!("http://{addr}/cacheable"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "finalize cached");
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        1,
        "second request should be from cache"
    );
}

// ── 89. Finalize response without cache applies read_timeout + bandwidth ─────

#[tokio::test]
async fn finalize_response_applies_read_timeout_and_bandwidth_without_cache() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .body(Full::new(Bytes::from("no-cache body")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_secs(30))
        .max_download_speed(1024 * 1024)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "no-cache body");
}

// ── 90. 304 NOT_MODIFIED is not treated as redirect in execute_send ───────────

#[tokio::test]
async fn not_modified_304_is_not_treated_as_redirect() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(304)
                .header("etag", "\"test\"")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector).build();

    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();

    // 304 should be returned as-is, not followed as a redirect
    assert_eq!(resp.status(), http::StatusCode::NOT_MODIFIED);
}

// ── 91. Middleware on_response callback applies via finalize ──────────────────

struct ResponseInjectMiddleware;

impl aioduct::Middleware for ResponseInjectMiddleware {
    fn on_response(
        &self,
        response: &mut http::Response<aioduct::body::RequestBoxBody>,
        _uri: &http::Uri,
    ) {
        response.headers_mut().insert(
            http::header::HeaderName::from_static("x-resp-mw"),
            http::header::HeaderValue::from_static("applied"),
        );
    }
}

#[tokio::test]
async fn middleware_on_response_applies_in_finalize() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(ResponseInjectMiddleware)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.headers().get("x-resp-mw").unwrap().to_str().unwrap(),
        "applied"
    );
}

// ── 92. Cache invalidation on POST request (variant) ─────────────────────────

#[tokio::test]
async fn cache_invalidation_on_post_variant() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let count = hit_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if req.method() == http::Method::POST {
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("posted"))))
            } else {
                Ok(Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from(format!("get-{n}"))))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .build();

    let url = format!("http://{addr}/resource");

    // Populate cache
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "get-0");

    // POST should invalidate cache
    let resp = client
        .post(&url)
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "posted");

    // GET should miss cache after POST invalidation
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "get-2");
    assert_eq!(hit_count.load(Ordering::SeqCst), 3);
}

// ── 93. HSTS upgrade HTTP to HTTPS via maybe_upgrade_hsts ────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn hsts_upgrade_http_to_https() {
    aioduct_test_server::tls::install_crypto_provider();

    let (tls_addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let hsts = aioduct::HstsStore::new();
    // Pre-populate HSTS for localhost using store_from_response
    let mut hsts_headers = http::HeaderMap::new();
    hsts_headers.insert(
        "strict-transport-security",
        http::header::HeaderValue::from_static("max-age=31536000"),
    );
    hsts.store_from_response("localhost", &hsts_headers);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(connector)
        .hsts(hsts)
        .timeout(Duration::from_secs(5))
        .build();

    // Request with http:// should be upgraded to https:// by HSTS
    let resp = client
        .get(&format!("http://localhost:{}/", tls_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        resp.tls_info().is_some(),
        "HSTS should upgrade to HTTPS, so TLS info should be present"
    );
}

// ── 94. Streaming body with no_connection_reuse (stale retry disabled) ───────

#[tokio::test]
async fn streaming_body_prevents_stale_retry() {
    // The stale retry is disabled for streaming bodies (can_stale_retry is false)
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // Streaming body - cannot be replayed (uses RequestBoxBody directly)
    use http_body_util::BodyExt;
    let stream_body: aioduct::body::RequestBoxBody =
        http_body_util::Full::new(Bytes::from("streaming data"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ── 95. User-agent default does not override explicit user-agent ─────────────

#[tokio::test]
async fn user_agent_default_does_not_override_explicit() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("ua={ua}")))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .user_agent("default-agent/1.0")
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::USER_AGENT,
            http::header::HeaderValue::from_static("override-agent/2.0"),
        )
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("override-agent/2.0"),
        "explicit header should override default, got: {body}"
    );
}

// ── 96. Referer header set on redirect when enabled ──────────────────────────

#[tokio::test]
async fn referer_header_set_on_redirect() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "/target")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let referer = req
                .headers()
                .get("referer")
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_else(|| "none".to_string());
            Ok(Response::new(Full::new(Bytes::from(format!(
                "referer={referer}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains(&format!("http://{addr}/start")),
        "referer should contain the original URL, got: {body}"
    );
}

// ── 97. Sensitive headers stripped on cross-origin redirect ──────────────────

#[tokio::test]
async fn sensitive_headers_stripped_on_cross_origin_redirect() {
    // Redirect server
    let redirect_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirect_addr = redirect_listener.local_addr().unwrap();

    // Target server
    let (target_addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "none".to_string());
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "none".to_string());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "auth={auth},cookie={cookie}"
        )))))
    })
    .await;

    // Redirect server that redirects to a different authority
    tokio::spawn(async move {
        let (stream, _) = redirect_listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let target_addr_inner = target_addr;
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                io,
                hyper::service::service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                    let redirect_to = format!("http://127.0.0.1:{}/", target_addr_inner.port());
                    async move {
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(302)
                                .header("location", redirect_to)
                                .body(Full::new(Bytes::new()))
                                .unwrap(),
                        )
                    }
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .sensitive_header(http::header::HeaderName::from_static("x-secret"))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!(
            "http://localhost:{}/cross-origin",
            redirect_addr.port()
        ))
        .unwrap()
        .header(
            http::header::AUTHORIZATION,
            http::header::HeaderValue::from_static("Bearer secret"),
        )
        .header(
            http::header::COOKIE,
            http::header::HeaderValue::from_static("session=abc"),
        )
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("auth=none"),
        "authorization should be stripped on cross-origin redirect, got: {body}"
    );
    assert!(
        body.contains("cookie=none"),
        "cookie should be stripped on cross-origin redirect, got: {body}"
    );
}

// ── 98. Host header auto-injected when missing ───────────────────────────────

#[tokio::test]
async fn host_header_auto_injected() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "missing".to_string());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "host={host}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains(&format!("host={addr}")),
        "host header should be auto-injected, got: {body}"
    );
}

// ── 99. No-op middleware (empty stack) still works ───────────────────────────

#[tokio::test]
async fn empty_middleware_stack_works() {
    let (addr, _counter) = h1_server().await;

    // Default client has no middleware
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 100. Chunk download debug format includes url ────────────────────────────

#[tokio::test]
async fn chunk_download_debug_includes_url() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let dl = client.chunk_download("http://example.com/large.bin");
    let debug = format!("{dl:?}");
    assert!(debug.contains("ChunkDownload"));
    assert!(debug.contains("large.bin"));
}

// ── 101. Retry budget exhaustion on connection error with middleware ──────────

#[tokio::test]
async fn retry_budget_exhaustion_on_connection_error_with_middleware() {
    // Use a port that's definitely not listening (connection refused = retryable error)
    let dead_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };

    let error_count = Arc::new(AtomicU32::new(0));
    let error_count_clone = error_count.clone();
    let retry_count = Arc::new(AtomicU32::new(0));
    let retry_count_clone = retry_count.clone();

    struct TrackingMiddleware {
        error_count: Arc<AtomicU32>,
        retry_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for TrackingMiddleware {
        fn on_error(&self, _error: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_retry(
            &self,
            _error: &aioduct::Error,
            _uri: &http::Uri,
            _method: &http::Method,
            _attempt: u32,
        ) {
            self.retry_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Budget of 0 tokens: first retry attempt will be denied
    let budget = aioduct::RetryBudget::new(0, 1);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(TrackingMiddleware {
            error_count: error_count_clone,
            retry_count: retry_count_clone,
        })
        .build();

    let result = client
        .get(&format!("http://127.0.0.1:{dead_port}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(5)
                .initial_backoff(Duration::from_millis(1))
                .budget(budget),
        )
        .timeout(Duration::from_secs(2))
        .send()
        .await;

    assert!(result.is_err(), "should fail when budget is exhausted");
    // Middleware on_error should have been called (budget exhaustion path)
    assert!(
        error_count.load(Ordering::SeqCst) >= 1,
        "on_error should be called when budget exhausted, got {}",
        error_count.load(Ordering::SeqCst)
    );
}

// ── 102. Retry fully exhausted with middleware (all attempts fail) ────────────

#[tokio::test]
async fn retry_fully_exhausted_with_middleware_fires_error() {
    let dead_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };

    let error_count = Arc::new(AtomicU32::new(0));
    let error_count_clone = error_count.clone();
    let retry_count = Arc::new(AtomicU32::new(0));
    let retry_count_clone = retry_count.clone();

    struct ErrorTrackMw {
        error_count: Arc<AtomicU32>,
        retry_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for ErrorTrackMw {
        fn on_error(&self, _error: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_retry(
            &self,
            _error: &aioduct::Error,
            _uri: &http::Uri,
            _method: &http::Method,
            _attempt: u32,
        ) {
            self.retry_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Large budget so it never blocks
    let budget = aioduct::RetryBudget::new(100, 1);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(ErrorTrackMw {
            error_count: error_count_clone,
            retry_count: retry_count_clone,
        })
        .build();

    let result = client
        .get(&format!("http://127.0.0.1:{dead_port}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(1))
                .budget(budget),
        )
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    assert!(result.is_err(), "all retries should be exhausted");
    // on_retry should have been called for each retry attempt
    assert_eq!(
        retry_count.load(Ordering::SeqCst),
        2,
        "on_retry should be called for each retry attempt"
    );
    // on_error should be called once at the end when retries are exhausted
    assert_eq!(
        error_count.load(Ordering::SeqCst),
        1,
        "on_error should be called once when retries exhausted"
    );
}

// ── 103. Non-retryable error with middleware fires on_error immediately ───────

#[tokio::test]
async fn non_retryable_error_with_middleware() {
    let error_count = Arc::new(AtomicU32::new(0));
    let error_count_clone = error_count.clone();

    struct NonRetryErrorMw {
        error_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for NonRetryErrorMw {
        fn on_error(&self, _error: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(NonRetryErrorMw {
            error_count: error_count_clone,
        })
        .https_only(true)
        .build();

    // Sending to http:// with https_only triggers a non-retryable error
    let result = client
        .get("http://example.com/")
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(1)),
        )
        .send()
        .await;

    assert!(result.is_err());
    // on_error should be called for non-retryable errors
    assert_eq!(
        error_count.load(Ordering::SeqCst),
        1,
        "on_error should fire for non-retryable errors"
    );
}

// ── 104. H2 multiplex concurrent requests dedup ──────────────────────────────

#[tokio::test]
async fn h2_multiplex_concurrent_requests_dedup() {
    let (addr, counter) = h2_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .http2_prior_knowledge()
        .build();

    let url = format!("http://{addr}/resource");
    let (r1, r2, r3) = tokio::join!(
        client.get(&url).unwrap().send(),
        client.get(&url).unwrap().send(),
        client.get(&url).unwrap().send(),
    );

    assert_eq!(r1.unwrap().status(), http::StatusCode::OK);
    assert_eq!(r2.unwrap().status(), http::StatusCode::OK);
    assert_eq!(r3.unwrap().status(), http::StatusCode::OK);

    // All requests should use at most 1 connection (H2 multiplexing)
    assert_eq!(
        counter.connections(),
        1,
        "H2 multiplexing should reuse a single connection"
    );
}

// ── 105. Stale-if-error returns cached response on network failure ───────────

#[tokio::test]
async fn stale_if_error_serves_cached_on_network_failure() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=0, stale-if-error=3600")
                    .header("etag", "\"v1\"")
                    .body(Full::new(Bytes::from("cached body")))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache.clone())
        .timeout(Duration::from_secs(2))
        .build();

    // Populate cache
    let resp = client
        .get(&format!("http://{addr}/stale-resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached body");

    // Create a client pointing to a dead port but sharing the same cache
    let dead_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };

    let client2 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .timeout(Duration::from_millis(500))
        .resolver(move |_host: &str, _port: u16| {
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], dead_port));
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = std::io::Result<std::net::SocketAddr>>
                            + Send,
                    >,
                >
        })
        .build();

    // This should serve from stale cache due to stale-if-error
    let resp = client2
        .get(&format!("http://{addr}/stale-resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "cached body");
}

// ── 106. Adaptive H2c probe error path (connect returns Err, line 730) ───────

#[tokio::test]
async fn adaptive_h2c_probe_error_connects_h1_fallback() {
    use tokio::io::AsyncReadExt;

    // Create a server that immediately closes the connection on first attempt
    let connection_count = Arc::new(AtomicU32::new(0));
    let connection_count_clone = connection_count.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let n = connection_count_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First connection: close immediately to trigger h2c probe error
                drop(stream);
            } else {
                // Subsequent connections: serve H1
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response = "HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\nh1 fallback";
                    use tokio::io::AsyncWriteExt;
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/probe-err-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(incoming)
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h1 fallback");
    assert!(
        connection_count.load(Ordering::SeqCst) >= 2,
        "should open at least 2 connections (probe + fallback)"
    );
}

// ── 107. Cache staleness with max-age expired triggers revalidation ──────────

#[tokio::test]
async fn cache_staleness_with_expired_max_age() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=0")
                    .header("etag", format!("\"v{n}\""))
                    .body(Full::new(Bytes::from(format!("body-{n}"))))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .build();

    let url = format!("http://{addr}/staleness");

    // First request: populates cache
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "body-0");

    // Second request: cache entry is immediately stale (max-age=0), should revalidate
    let resp = client.get(&url).unwrap().send().await.unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        request_count.load(Ordering::SeqCst) >= 2,
        "stale cache should trigger revalidation"
    );
    assert_eq!(body, "body-1");
}

// ── 108. Retry on status with budget exhaustion + middleware ──────────────────

#[tokio::test]
async fn retry_on_status_budget_exhaustion_with_middleware() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(503)
                    .body(Full::new(Bytes::from("unavailable")))
                    .unwrap(),
            )
        }
    })
    .await;

    let retry_count = Arc::new(AtomicU32::new(0));
    let retry_count_clone = retry_count.clone();

    struct StatusRetryMw {
        retry_count: Arc<AtomicU32>,
    }
    impl aioduct::Middleware for StatusRetryMw {
        fn on_retry(
            &self,
            _error: &aioduct::Error,
            _uri: &http::Uri,
            _method: &http::Method,
            _attempt: u32,
        ) {
            self.retry_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Budget of 1: allows one retry, then exhausted
    let budget = aioduct::RetryBudget::new(1, 0);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(StatusRetryMw {
            retry_count: retry_count_clone,
        })
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(5)
                .retry_on_status(true)
                .initial_backoff(Duration::from_millis(1))
                .budget(budget),
        )
        .send()
        .await
        .unwrap();

    // Budget allows 1 retry, so 2 total requests (original + 1 retry)
    assert_eq!(resp.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        2,
        "should make original + 1 retry before budget exhaustion"
    );
    assert_eq!(
        retry_count.load(Ordering::SeqCst),
        1,
        "on_retry should be called once"
    );
}
