use super::*;

#[tokio::test]
async fn https_only_rejects_http_url() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .https_only(true)
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(jar)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-middleware"),
                    http::header::HeaderValue::from_static("dispatch-test"),
                );
            },
        )
        .build()
        .unwrap();

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
pub struct TestObserver {
    pub phases: Arc<Mutex<Vec<String>>>,
    pub conn_events: Arc<Mutex<Vec<String>>>,
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
            RequestPhase::Redirected { .. } => "Redirected".to_string(),
            RequestPhase::Retrying { .. } => "Retrying".to_string(),
            RequestPhase::TrailersReceived { .. } => "TrailersReceived".to_string(),
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(50))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_download_speed(1024 * 1024)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_requests_per_sec(100)
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .hsts(hsts)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_decompression()
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .resolve("custom.local", addr)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_redirects(3)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_millis(1))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

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
