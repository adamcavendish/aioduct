#[path = "chunk_download.rs"]
mod chunk_download;
#[path = "retry_middleware.rs"]
mod retry_middleware;
use super::*;
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/api/v1/users")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_millis(30))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/hook-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
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

// ── 79. Builder with min_tls_version exercises TLS version branch ────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn builder_min_tls_version_builds_successfully() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .min_tls_version(aioduct::TlsVersion::Tls1_3)
        .build()
        .unwrap();

    // Just verify it builds without panic; actual TLS connection tested elsewhere
    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    // Will fail to connect (port 1), but verifies construction
    assert!(result.is_err());
}

// ── 80. Builder with max_tls_version ─────────────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn builder_max_tls_version_builds_successfully() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_tls_version(aioduct::TlsVersion::Tls1_2)
        .build()
        .unwrap();

    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    assert!(result.is_err());
}

// ── 81. Builder with tls_sni disabled exercises SNI path ─────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn builder_tls_sni_disabled_builds_successfully() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls_sni(false)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let result = client
        .forward(super::valid_forward_request(incoming))
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/users/123")
        .header("host", "original.example.com")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/test")
        .header("authorization", "Bearer token123")
        .header("cookie", "session=abc")
        .header("x-forwarded", "original-value")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/rpc/method")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .read_timeout(Duration::from_secs(30))
        .max_download_speed(1024 * 1024)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_secs(30))
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();

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
        response: &mut http::Response<aioduct::body::RequestBodySend>,
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(ResponseInjectMiddleware)
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .hsts(hsts)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Streaming body - cannot be replayed (uses RequestBodySend directly)
    use http_body_util::BodyExt;
    let stream_body: aioduct::body::RequestBodySend =
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .user_agent("default-agent/1.0")
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .sensitive_header(http::header::HeaderName::from_static("x-secret"))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let dl = client.chunk_download("http://example.com/large.bin");
    let debug = format!("{dl:?}");
    assert!(debug.contains("ChunkDownloadSend"));
    assert!(debug.contains("large.bin"));
}

// ── 104. H2 multiplex concurrent requests dedup ──────────────────────────────

#[tokio::test]
async fn h2_multiplex_concurrent_requests_dedup() {
    let (addr, counter) = h2_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let url = format!("http://{addr}/resource");
    let (r1, r2, r3) = tokio::join!(
        client.get(&url).unwrap().h2c_prior_knowledge().send(),
        client.get(&url).unwrap().h2c_prior_knowledge().send(),
        client.get(&url).unwrap().h2c_prior_knowledge().send(),
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache.clone())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

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

    let client2 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
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
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/probe-err-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(StatusRetryMw {
            retry_count: retry_count_clone,
        })
        .build()
        .unwrap();

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
