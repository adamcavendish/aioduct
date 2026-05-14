#![cfg(feature = "tokio")]

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

use aioduct_test_server::h1::{h1_server, h1_server_with};
use aioduct_test_server::raw::raw_server;

#[tokio::test]
async fn test_connection_refused() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    assert!(result.is_err());
}
#[tokio::test]
async fn test_client_clone_shares_pool() {
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

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

    let (addr, _counter) = h1_server_with(move |_req| {
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
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

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
    let (addr, _counter) = h1_server_with(move |_req| {
        let body = body_clone.clone();
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body)))) }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);

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
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (final_addr, _counter) = h1_server().await;
    let (redirect_addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let dbg = format!("{client:?}");
    assert!(dbg.contains("HttpEngineSend"));
}
#[tokio::test]
async fn test_rate_limiter_throttles() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

    let (addr, _counter) = h1_server_with(move |_req| {
        let data = data_clone.clone();
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(data)))) }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

#[tokio::test]
async fn overridden_dns_resolution() {
    use std::pin::Pin;

    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("resolved"))))
    })
    .await;

    let overridden_domain = "rust-lang.test";
    let port = addr.port();

    let target_addr = addr;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .resolver(
            move |_host: &str,
                  _port: u16|
                  -> Pin<
                Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
            > {
                let target = target_addr;
                Box::pin(async move { Ok(target) })
            },
        )
        .build();

    let url = format!("http://{overridden_domain}:{port}/");
    let resp = client.get(&url).unwrap().send().await.unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let text = resp.text().await.unwrap();
    assert_eq!(text, "resolved");
}

#[tokio::test]
async fn resolve_builder_convenience() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
            "resolved via builder",
        ))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .resolve("my-custom-host.test", addr)
        .build();

    let url = format!("http://my-custom-host.test:{}/path", addr.port());
    let resp = client.get(&url).unwrap().send().await.unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "resolved via builder");
}

#[tokio::test]
async fn resolve_to_addrs_builder_convenience() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("multi-addr"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .resolve_to_addrs("multi.test", &[addr])
        .build();

    let url = format!("http://multi.test:{}/", addr.port());
    let resp = client.get(&url).unwrap().send().await.unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "multi-addr");
}

#[tokio::test]
async fn close_connection_after_idle_timeout() {
    let request_count = Arc::new(AtomicU32::new(0));
    let count_clone = request_count.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let count = count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_millis(100))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn error_connection_refused_with_url() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let result = client.get("http://127.0.0.1:1/path").unwrap().send().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.is_connect(), "expected connect error, got: {err:?}");
}

#[tokio::test]
async fn error_for_status_with_reason_phrase() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .status(418)
                .body(Full::new(Bytes::new()))
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

    assert_eq!(resp.status().as_u16(), 418);
    let err = resp.error_for_status().unwrap_err();
    assert!(err.is_status());
    assert_eq!(err.status(), Some(http::StatusCode::from_u16(418).unwrap()));
}

#[tokio::test]
async fn error_carries_url_context() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let err = client
        .get("http://127.0.0.1:1/the-path")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert_eq!(err.url().path(), "/the-path");
    assert!(err.is_connect());

    let display = format!("{err}");
    assert!(
        display.contains("/the-path"),
        "Display should include URL, got: {display}"
    );

    let inner: aioduct::Error = err.into();
    assert!(inner.is_connect());
}

#[tokio::test]
async fn error_for_status_display_includes_status_code() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .status(418)
                .body(Full::new(Bytes::new()))
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

    let err = resp.error_for_status().unwrap_err();
    let display = format!("{err}");
    assert!(
        display.contains("418"),
        "error_for_status Display should include the status code, got: {display}"
    );
}

#[tokio::test]
async fn send_error_display_includes_url_for_connection_error() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let err = client
        .get("http://127.0.0.1:1/test-path")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    let display = format!("{err}");
    assert!(
        display.contains("/test-path"),
        "SendError Display should include URL, got: {display}"
    );
}

#[tokio::test]
async fn user_agent_builder() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .user_agent("aioduct-test/1.0")
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "aioduct-test/1.0");
}

#[tokio::test]
async fn default_headers_applied() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-custom-default")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
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
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "default-value");
}

#[tokio::test]
async fn request_header_overrides_default_header() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::header::HeaderValue::from_static("default-token"),
    );
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .default_headers(headers)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::AUTHORIZATION,
            http::header::HeaderValue::from_static("override-token"),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "override-token");
}

#[tokio::test]
async fn http1_reason_phrase_in_status() {
    let addr = raw_server(|_req| async {
        b"HTTP/1.1 418 I'm a Teapot\r\nContent-Length: 0\r\n\r\n".to_vec()
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 418);
}
