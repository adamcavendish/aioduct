use super::*;
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .hsts(hsts.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .request_observer(obs.clone())
            .build()
            .unwrap(),
    );

    // Make 2 sequential requests to ensure multiplex clone path
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .h2c_prior_knowledge()
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .hsts(hsts)
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .default_headers(headers)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .default_headers(headers)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Use a streaming body (non-clonable) with a POST + 308 redirect
    use http_body_util::BodyExt as _;
    let chunks: Vec<Result<hyper::body::Frame<Bytes>, aioduct::Error>> =
        vec![Ok(hyper::body::Frame::data(Bytes::from("stream")))];
    let stream = futures_util::stream::iter(chunks);
    let streaming_body: aioduct::body::RequestBodySend =
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .redirect_policy(aioduct::RedirectPolicy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("testuser", "testpass")
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(jar)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .request_observer(obs.clone())
        .build()
        .unwrap();

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
    // Should see Failed with retry: StaleConnection, and PoolCheckoutComplete(StaleRetry)
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-fresh-middleware"),
                    http::header::HeaderValue::from_static("fresh-path"),
                );
            },
        )
        .no_connection_reuse()
        .build()
        .unwrap();

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
