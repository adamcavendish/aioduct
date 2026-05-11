#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_connection_refused() {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    assert!(result.is_err());
}
#[tokio::test]
async fn test_client_clone_shares_pool() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let cloned = client.clone();

    let resp1 = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp1.text().await.unwrap();

    let resp2 = cloned
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), http::StatusCode::OK);
    let body = resp2.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}
#[tokio::test]
async fn test_concurrent_requests() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let mut handles = Vec::new();
    for _ in 0..10 {
        let client = client.clone();
        let url = format!("http://{addr}/");
        handles.push(tokio::spawn(async move {
            client
                .get(&url)
                .unwrap()
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }));
    }

    for handle in handles {
        let body = handle.await.unwrap();
        assert_eq!(body, "hello aioduct");
    }
}
#[tokio::test]
async fn test_no_connection_reuse() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let addr = start_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .no_connection_reuse()
        .build();

    for _ in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await;
    }
    assert_eq!(request_count.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn test_remote_addr_is_set() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let remote = resp.remote_addr();
    assert!(remote.is_some(), "remote_addr should be set");
    assert_eq!(remote.unwrap().port(), addr.port());
}
#[tokio::test]
async fn test_response_content_length() {
    let body = "x".repeat(42);
    let body_clone = body.clone();
    let addr = start_server_with(move |_req| {
        let body = body_clone.clone();
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body)))) }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(42));
}
#[tokio::test]
async fn test_response_version() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.version(), http::Version::HTTP_11);
}
#[tokio::test]
async fn test_error_for_status_integration() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/missing"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    let result = resp.error_for_status();
    assert!(result.is_err());
}
#[tokio::test]
async fn test_response_url_after_redirect() {
    let final_addr = start_server().await;
    let redirect_addr = start_server_with(move |_req| {
        let target = format!("http://{final_addr}/final");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let url = resp.url().to_string();
    assert!(
        url.contains("/final"),
        "url should reflect final destination after redirect, got: {url}"
    );
}
#[tokio::test]
async fn test_client_debug() {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let dbg = format!("{client:?}");
    assert!(dbg.contains("HttpEngine"));
}
#[tokio::test]
async fn test_rate_limiter_throttles() {
    let addr = start_server().await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .rate_limiter(aioduct::RateLimiter::new(100, Duration::from_secs(1)))
        .build();

    let start = tokio::time::Instant::now();
    for _ in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await;
    }
    let elapsed = start.elapsed();
    // 100 req/sec → ~10ms per request. 3 requests should be fast.
    assert!(elapsed < Duration::from_secs(1));
}
#[tokio::test]
async fn test_rate_limiter_sleep_path() {
    let addr = start_server().await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .rate_limiter(aioduct::RateLimiter::new(1, Duration::from_millis(200)))
        .build();

    let start = tokio::time::Instant::now();
    for i in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            http::StatusCode::OK,
            "request {i} should succeed"
        );
        let _ = resp.text().await;
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(300),
        "1 req per 200ms → 3 requests should take at least 300ms, got {elapsed:?}"
    );
}
#[tokio::test]
async fn test_bandwidth_limiter_download() {
    let data = "x".repeat(500);
    let data_clone = data.clone();

    let addr = start_server_with(move |_req| {
        let data = data_clone.clone();
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(data)))) }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_download_speed(100_000)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(client.bandwidth_limiter().is_some());
    let body = resp.text().await.unwrap();
    assert_eq!(body.len(), 500);
}
#[tokio::test]
async fn test_https_only_rejects_http() {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .https_only(true)
        .build();

    let result = client.get("http://example.com/").unwrap().send().await;
    assert!(result.is_err());
    let err = format!("{:?}", result.unwrap_err());
    assert!(
        err.contains("HttpsOnly") || err.contains("http"),
        "expected https-only error, got: {err}"
    );
}
