use super::*;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 5. Cache invalidation on non-GET methods
//    Exercises execute_send.rs:167-169 (cache.invalidate).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn cache_invalidated_by_post_request() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let count = hit_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            if method == "GET" {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=3600")
                        .body(Full::new(Bytes::from(format!("get-response-{n}"))))
                        .unwrap(),
                )
            } else {
                Ok(Response::builder()
                    .body(Full::new(Bytes::from(format!(
                        "post-response method={method} path={path}"
                    ))))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First GET: stores in cache
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "get-response-0");
    assert_eq!(hit_count.load(Ordering::SeqCst), 1);

    // Second GET: served from cache (no server hit)
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "get-response-0");
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        1,
        "cache should serve second GET"
    );

    // POST to same URL: should invalidate cache
    let resp = client
        .post(&format!("http://{addr}/resource"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("post-response"),
        "POST should succeed, got: {body}"
    );

    // Third GET: cache was invalidated, should hit server again
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "get-response-2",
        "cache should be invalidated after POST"
    );
    assert_eq!(hit_count.load(Ordering::SeqCst), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 10. Stale-if-error: server error path with stale cache entry
//     Exercises execute_send.rs:113-130 (server returns 5xx, stale cache serves).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn stale_if_error_serves_stale_on_503() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: cacheable response with stale-if-error
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, stale-if-error=3600")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("fresh-data")))
                        .unwrap(),
                )
            } else {
                // Subsequent: verify revalidation header, return 503
                let has_inm = req.headers().contains_key("if-none-match");
                assert!(has_inm, "revalidation should send If-None-Match");
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
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: populate cache
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "fresh-data");

    // Second request: server returns 503, stale-if-error should serve cached data
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "stale-if-error should serve stale cache on 503"
    );
    assert_eq!(
        resp.text().await.unwrap(),
        "fresh-data",
        "stale-if-error should serve original cached body"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 11. Stale-if-error: connection error path with stale cache entry
//     Exercises execute_send.rs:133-140 (error with stale cache fallback).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn stale_if_error_serves_stale_on_connection_failure() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=0, stale-if-error=3600")
                .header("etag", "\"conn-v1\"")
                .body(Full::new(Bytes::from("originally-cached")))
                .unwrap(),
        )
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Populate cache
    let resp = client
        .get(&format!("http://{addr}/stale-conn"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "originally-cached");

    // Build a new client pointing at a dead port but using the same cache
    let dead_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let client2 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .timeout(Duration::from_secs(2))
        .resolver(move |_host: &str, _port: u16| {
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], dead_port));
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                >
        })
        .build()
        .unwrap();

    // Request to dead port: should serve stale cached data
    let resp = client2
        .get(&format!("http://{addr}/stale-conn"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "stale-if-error should serve cached data when connection fails"
    );
    assert_eq!(resp.text().await.unwrap(), "originally-cached");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 15. finalize_response caches a cacheable response
//     Exercises execute_send.rs:303-311 (cache.store path in finalize_response).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn finalize_response_stores_cacheable_response() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let count = hit_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from(format!("cacheable-{n}"))))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: finalize_response should store the response
    let resp = client
        .get(&format!("http://{addr}/cached"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "cacheable-0");

    // Second request: should come from cache (no server hit)
    let resp = client
        .get(&format!("http://{addr}/cached"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        "cacheable-0",
        "second request should serve from cache"
    );
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        1,
        "server should only be hit once due to caching"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 16. Cookie jar stores cookies from response
//     Exercises execute_send.rs:171-175 (cookie jar store path).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn cookie_jar_stores_set_cookie_from_response() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let cookie_header = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        if cookie_header.is_empty() {
            // First request: set a cookie
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "session=abc123; Path=/")
                    .body(Full::new(Bytes::from("cookie-set")))
                    .unwrap(),
            )
        } else {
            // Subsequent requests: echo back the cookie
            Ok(Response::new(Full::new(Bytes::from(format!(
                "cookie={cookie_header}"
            )))))
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(jar)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: server sets a cookie
    let resp = client
        .get(&format!("http://{addr}/page"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "cookie-set");

    // Second request: cookie jar should send the cookie back
    let resp = client
        .get(&format!("http://{addr}/page"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("session=abc123"),
        "cookie jar should send stored cookie, got: {body}"
    );
}
