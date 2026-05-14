#![cfg(feature = "tokio")]

mod common;
use common::*;

// =============================================================================
// Per-Forward h2c and Adaptive h2c Tests
// =============================================================================

#[tokio::test]
async fn forward_h2c_to_h2_upstream() {
    let addr = start_h2_server_with(|req| async move {
        let version = format!("{:?}", req.version());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(version))))
    })
    .await;

    // Client without http2_prior_knowledge — uses h1 by default
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("POST")
        .uri("/grpc.Service/Method")
        .body(Full::new(Bytes::from("request body")))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "HTTP/2.0");
}

#[tokio::test]
async fn forward_adaptive_h2c_to_h2_server() {
    let addr = start_h2_server_with(|req| async move {
        let version = format!("{:?}", req.version());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(version))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    // First request: probes h2c, should succeed
    let req1 = http::Request::builder()
        .method("POST")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req1)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "HTTP/2.0");

    // Second request: should hit cache (no re-probe), still succeeds
    let req2 = http::Request::builder()
        .method("POST")
        .uri("/test2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req2)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "HTTP/2.0");
}

#[tokio::test]
async fn forward_adaptive_h2c_falls_back_to_h1() {
    // Start an h1-only server
    let addr = start_server_with(|req| async move {
        let version = format!("{:?}", req.version());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(version))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    // First request: probes h2c, fails, falls back to h1
    let req1 = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req1)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "HTTP/1.1");

    // Second request: cache says h1-only, goes straight to h1
    let req2 = http::Request::builder()
        .method("GET")
        .uri("/test2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req2)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "HTTP/1.1");
}

#[tokio::test]
async fn forward_h2c_preserves_request_body() {
    let addr = start_h2_server_with(|req| async move {
        use http_body_util::BodyExt;
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(body)))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let payload = "hello gRPC body content";
    let req = http::Request::builder()
        .method("POST")
        .uri("/grpc.Service/Echo")
        .body(Full::new(Bytes::from(payload)))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), payload);
}

#[tokio::test]
async fn forward_h2c_rewrites_host_and_preserves_custom_headers() {
    let addr = start_h2_server_with(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let custom = req
            .headers()
            .get("x-custom")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "host={host},custom={custom}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("Host", "original.example.com")
        .header("X-Custom", "kept")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    // Host should be rewritten to the upstream authority
    assert!(
        text.contains(&format!("host=127.0.0.1:{}", addr.port())),
        "expected rewritten host, got: {text}"
    );
    assert!(text.contains("custom=kept"), "custom header lost: {text}");
}

#[tokio::test]
async fn forward_h2c_with_strip_prefix() {
    let addr = start_h2_server_with(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/api/v1/resource")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .strip_prefix("/api")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "/v1/resource");
}

#[tokio::test]
async fn forward_h2c_with_upstream_base_path() {
    let addr = start_h2_server_with(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/items")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}/v2", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "/v2/items");
}

#[tokio::test]
async fn forward_h2c_extra_and_remove_headers() {
    let addr = start_h2_server_with(|req| async move {
        let added = req
            .headers()
            .get("x-added")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let removed = req.headers().contains_key("x-remove-me");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "added={added},removed={removed}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("X-Remove-Me", "should be gone")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .header(
            http::header::HeaderName::from_static("x-added"),
            http::header::HeaderValue::from_static("injected"),
        )
        .remove_header(http::header::HeaderName::from_static("x-remove-me"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "added=injected,removed=false");
}

#[tokio::test]
async fn forward_h2c_on_request_hook() {
    let addr = start_h2_server_with(|req| async move {
        let via = req
            .headers()
            .get("x-hook")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(via))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .on_request(|parts| {
            parts.headers.insert("x-hook", "from-hook".parse().unwrap());
        })
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "from-hook");
}

#[tokio::test]
async fn forward_h2c_on_response_hook() {
    let addr = start_h2_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .on_response(|resp| {
            resp.headers_mut()
                .insert("x-from-hook", "yes".parse().unwrap());
        })
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.headers().get("x-from-hook").unwrap().to_str().unwrap(),
        "yes"
    );
}

#[tokio::test]
async fn forward_h2c_timeout() {
    use tokio::sync::Notify;

    let notify = Arc::new(Notify::new());
    let notify_clone = notify.clone();

    let addr = start_h2_server_with(move |_req| {
        let n = notify_clone.clone();
        async move {
            n.notified().await;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("too late"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/slow")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .timeout(Duration::from_millis(50))
        .send()
        .await;

    assert!(result.is_err(), "expected timeout error");
    notify.notify_waiters();
}

#[tokio::test]
async fn forward_h2c_preserve_host() {
    let addr = start_h2_server_with(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("Host", "original.example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .preserve_host()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "original.example.com");
}

#[tokio::test]
async fn forward_adaptive_h2c_probe_cache_isolates_authorities() {
    let h2_addr = start_h2_server_with(|req| async move {
        let version = format!("{:?}", req.version());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(version))))
    })
    .await;

    let h1_addr = start_server_with(|req| async move {
        let version = format!("{:?}", req.version());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(version))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    // Probe h2 server — should cache as h2c-capable
    let req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", h2_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "HTTP/2.0");

    // Probe h1 server — should cache as h1-only (separate authority)
    let req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", h1_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "HTTP/1.1");

    // Second request to h2 server — should use cached h2c (no re-probe)
    let req = http::Request::builder()
        .method("GET")
        .uri("/test2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", h2_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "HTTP/2.0");
}

#[tokio::test]
async fn forward_h2c_query_string_preserved() {
    let addr = start_h2_server_with(|req| async move {
        let pq = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(pq))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let req = http::Request::builder()
        .method("GET")
        .uri("/search?q=grpc&limit=10")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req)
        .upstream(
            format!("http://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "/search?q=grpc&limit=10");
}

#[tokio::test]
async fn forward_mixed_h1_h2c_pool_isolation() {
    // Start both an h1 server and an h2 server
    let h1_addr = start_server_with(|req| async move {
        let version = format!("{:?}", req.version());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(version))))
    })
    .await;

    let h2_addr = start_h2_server_with(|req| async move {
        let version = format!("{:?}", req.version());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(version))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

    // Request 1: forward to h1 upstream (no h2c)
    let req_h1 = http::Request::builder()
        .method("GET")
        .uri("/h1")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req_h1)
        .upstream(
            format!("http://127.0.0.1:{}", h1_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "HTTP/1.1");

    // Request 2: forward to h2 upstream with .h2c()
    let req_h2 = http::Request::builder()
        .method("POST")
        .uri("/h2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req_h2)
        .upstream(
            format!("http://127.0.0.1:{}", h2_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "HTTP/2.0");

    // Request 3: forward to h1 upstream again — pool must NOT return an h2 connection
    let req_h1_again = http::Request::builder()
        .method("GET")
        .uri("/h1-again")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(req_h1_again)
        .upstream(
            format!("http://127.0.0.1:{}", h1_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "HTTP/1.1");
}
