use super::*;

#[test]
fn test_compio_cache_basic() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=3600")
                .body(Full::new(Bytes::from("cached response")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.text().await.unwrap(), "cached response");

        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.text().await.unwrap(), "cached response");
    });
}

// ── Cache stale-if-error tests ─────────────────────────────────────

#[test]
fn test_compio_cache_stale_if_error_on_5xx() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |_req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: return cacheable response with short max-age and stale-if-error
                Ok::<_, Infallible>(
                    Response::builder()
                        .header(
                            "cache-control",
                            "max-age=0, must-revalidate, stale-if-error=3600",
                        )
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("fresh data")))
                        .unwrap(),
                )
            } else {
                // Subsequent requests: return 500
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("server error")))
                        .unwrap(),
                )
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        // First request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "fresh data");

        // Small delay to ensure max-age=0 makes the entry stale
        std::thread::sleep(Duration::from_millis(10));

        // Second request: server returns 500, stale-if-error should serve cached
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "fresh data");
    });
}

#[test]
fn test_compio_cache_stale_if_error_on_network_error() {
    // Start server, cache a response, then shut it down, verify stale cache is served.
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();

            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        let (stream, _) = accept_result.unwrap();
                        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                        tokio::spawn(async move {
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(
                                    io,
                                    service_fn(|_req| async {
                                        Ok::<_, Infallible>(
                                            Response::builder()
                                                .header(
                                                    "cache-control",
                                                    "max-age=0, must-revalidate, stale-if-error=3600",
                                                )
                                                .header("etag", "\"v1\"")
                                                .body(Full::new(Bytes::from("cached from server")))
                                                .unwrap(),
                                        )
                                    }),
                                )
                                .await;
                        });
                    }
                    _ = tokio::task::spawn_blocking(|| { /* yield */ }) => {}
                }
                // Check if shutdown was signaled
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
            }
        });
    });

    let addr = addr_rx.recv().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .timeout(Duration::from_millis(500))
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        // First request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "cached from server");

        // Small delay to ensure max-age=0 makes the entry stale
        std::thread::sleep(Duration::from_millis(10));

        // Shut down server
        let _ = shutdown_tx.send(());
        std::thread::sleep(Duration::from_millis(50));

        // Second request: server is down, stale-if-error should serve cached
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "cached from server");
    });
}

// ── Cache 304 revalidation test ────────────────────────────────────

#[test]
fn test_compio_cache_304_revalidation() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: return cacheable response with ETag and short max-age
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, must-revalidate")
                        .header("etag", "\"abc123\"")
                        .body(Full::new(Bytes::from("original content")))
                        .unwrap(),
                )
            } else {
                // Subsequent requests: check If-None-Match, return 304
                let if_none_match = req
                    .headers()
                    .get("if-none-match")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if if_none_match == "\"abc123\"" {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(304)
                            .header("etag", "\"abc123\"")
                            .body(Full::new(Bytes::new()))
                            .unwrap(),
                    )
                } else {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .body(Full::new(Bytes::from("unexpected")))
                            .unwrap(),
                    )
                }
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        // First request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "original content");

        // Small delay to ensure max-age=0 makes the entry stale
        std::thread::sleep(Duration::from_millis(10));

        // Second request: should revalidate with If-None-Match, get 304, serve cached
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "original content");
    });
}

// ── Cache invalidation on write test ───────────────────────────────

#[test]
fn test_compio_cache_invalidation_on_post() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            let method = req.method().clone();
            match method {
                ref m if *m == http::Method::GET => Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=3600")
                        .body(Full::new(Bytes::from(format!("get response #{n}"))))
                        .unwrap(),
                ),
                _ => Ok::<_, Infallible>(
                    Response::builder()
                        .status(200)
                        .body(Full::new(Bytes::from("post ok")))
                        .unwrap(),
                ),
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/resource");

        // GET request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let body1 = resp1.text().await.unwrap();
        assert!(body1.contains("get response"), "body: {body1}");

        // GET again: should be cached (same content)
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.text().await.unwrap(), body1);

        // POST: should invalidate cache
        let resp3 = client
            .post_local(&url)
            .unwrap()
            .body("data")
            .send()
            .await
            .unwrap();
        assert_eq!(resp3.status(), http::StatusCode::OK);
        let _ = resp3.text().await.unwrap();

        // GET again: cache was invalidated, should get fresh response
        let resp4 = client.get_local(&url).unwrap().send().await.unwrap();
        let body4 = resp4.text().await.unwrap();
        assert_ne!(body4, body1, "cache should have been invalidated by POST");
    });
}
