#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{h1_server, h1_server_with};

#[tokio::test]
async fn test_request_timeout_triggers() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(50))
        .send()
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected Timeout error, got: {err:?}");
}

#[tokio::test]
async fn test_request_timeout_completes_in_time() {
    let (addr, _counter) = h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_client_default_timeout_triggers() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err());
    assert!(result.unwrap_err().is_timeout());
}

#[tokio::test]
async fn test_request_timeout_overrides_client_timeout() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("delayed"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(10))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "delayed");
}
#[tokio::test]
async fn test_read_timeout_does_not_apply_to_headers() {
    // Note: aioduct's read_timeout only applies to body reads, not header wait.
    // Use request timeout for header wait timeouts.
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow headers"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "slow headers");
}

#[tokio::test]
async fn test_read_timeout_applies_to_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = stream.write_all(b"world").await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
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
        "read_timeout should fire on slow body chunks"
    );
}

#[tokio::test]
async fn test_read_timeout_allows_slow_but_steady_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        for i in 0..3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let chunk = format!("1\r\n{i}\r\n");
            stream.write_all(chunk.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        }

        stream.write_all(b"0\r\n\r\n").await.unwrap();
        stream.flush().await.unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(200))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "012", "slow-but-within-threshold body should succeed");
}

#[tokio::test]
async fn test_content_length_preserved_through_timeout() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "5")
                .body(Full::new(Bytes::from("hello")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(5));
}

#[tokio::test]
async fn test_connect_timeout() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let start = tokio::time::Instant::now();
    let result = client
        .get("http://192.0.2.1:81/slow")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    assert!(result.is_err(), "connect_timeout should fire");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "should timeout quickly, not wait for request timeout"
    );
}

#[tokio::test]
async fn client_timeout_triggers_on_slow_response() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected timeout, got: {err:?}");
}

#[tokio::test]
async fn per_request_timeout_triggers_on_slow_response() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(100))
        .send()
        .await;

    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected timeout, got: {err:?}");
}

#[tokio::test]
async fn connect_timeout_with_unreachable_ip() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let result = client
        .get("http://192.0.2.1:81/slow")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    let err = result.unwrap_err();
    assert!(
        err.is_timeout() || err.is_connect(),
        "expected timeout or connect error, got: {err:?}"
    );
}

#[tokio::test]
async fn read_timeout_does_not_apply_to_headers() {
    // Unlike reqwest, aioduct's read_timeout only applies to body reads.
    // Use request timeout for header wait timeouts.
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow headers"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
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
    assert_eq!(resp.text().await.unwrap(), "slow headers");
}

#[tokio::test]
async fn request_timeout_overrides_client_timeout() {
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("delayed"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "delayed");
}

#[tokio::test]
async fn timeout_fast_response_succeeds() {
    let (addr, _counter) = h1_server().await;

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

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.content_length(), Some(13));
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello aioduct");
}

#[tokio::test]
async fn connect_timeout_does_not_affect_fast_connects() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
}

// ── Edge-Case Timeout Tests ─────────────────────────────────────────────

// 1. Per-request connect_timeout without client-level connect_timeout.
#[tokio::test]
async fn connect_timeout_per_request() {
    // Use TEST-NET-1 (RFC 5737) — guaranteed unroutable, TCP connect will time out.
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let result = client
        .get("http://192.0.2.1:81/unreachable")
        .unwrap()
        .connect_timeout(Duration::from_millis(100))
        .send()
        .await;

    assert!(
        result.is_err(),
        "per-request connect_timeout should produce an error for unroutable IP"
    );
    let err = result.unwrap_err();
    assert!(
        err.is_timeout() || err.is_connect(),
        "expected timeout or connect error, got: {err:?}"
    );
}

// 2. Timeout fires during body upload when server reads slowly.
#[tokio::test]
async fn timeout_during_body_upload() {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Server: accept connection, read only headers, then delay reading body.
    // This causes TCP send buffer backpressure on the client side.
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let mut total = 0;
        loop {
            let n = stream.read(&mut buf[total..]).await.unwrap();
            if n == 0 {
                return;
            }
            total += n;
            if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // Server has read headers but intentionally does not read the body.
        // Sleep to keep the connection open while client uploads.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();

    // Large streaming body: enough chunks to fill TCP send buffers.
    use http_body_util::BodyExt;
    let chunk = Bytes::from(vec![b'X'; 65536]);
    let num_chunks = 200;
    let chunks: Vec<_> = (0..num_chunks)
        .map(|_| Ok(hyper::body::Frame::data(chunk.clone())))
        .collect();
    let stream = futures_util::stream::iter(chunks);
    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::StreamBody::new(stream).boxed_unsync();

    let result = client
        .post(&format!("http://{addr}/upload"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await;

    assert!(result.is_err(), "timeout should fire during body upload");
    let err = result.unwrap_err();
    assert!(
        err.is_timeout(),
        "expected timeout during upload, got: {err:?}"
    );
}

// 3. After a request times out, the timed-out connection is not returned to the pool.
#[tokio::test]
async fn timeout_cancellation_does_not_pool_broken_connection() {
    let slow_req_count = Arc::new(AtomicUsize::new(0));
    let rc = Arc::clone(&slow_req_count);

    let (addr, _counter) = h1_server_with(move |_req| {
        let n = rc.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 1 {
                // Second request (n=1): sleep past the client timeout.
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();

    // Prime the pool with one connection.
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _body = resp.text().await.unwrap();
    // Allow time for connection to be returned to pool.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request uses the pooled connection but times out mid-response.
    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
    assert!(result.is_err(), "second request should time out");
    assert!(result.unwrap_err().is_timeout());
    // Allow time for pool eviction.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // After timeout, a new request should still succeed (fresh connection).
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _body = resp.text().await.unwrap();
}

// 4. Timeout covers the entire redirect chain, not reset per hop.
#[tokio::test]
async fn timeout_during_redirect_chain() {
    // Server B: slow — sleeps 500 ms before responding.
    let (slow_addr, _slow_counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow response"))))
    })
    .await;

    // Server A: redirects to the slow Server B immediately.
    let (redirect_addr, _redirect_counter) = h1_server_with(move |_req| {
        let target = format!("http://{slow_addr}/slow");
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();

    // The timeout covers the redirect hop + slow response — 200 ms < 500 ms.
    let result = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await;

    assert!(
        result.is_err(),
        "200ms timeout should fire before 500ms redirect chain completes"
    );
    let err = result.unwrap_err();
    assert!(
        err.is_timeout(),
        "expected timeout bounding entire redirect chain, got: {err:?}"
    );

    // With a generous per-request timeout the same redirect chain succeeds.
    let resp = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "slow response");
}

// 5a. connect_timeout fires independently from overall timeout.
#[tokio::test]
async fn connect_timeout_independent_of_overall_timeout() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_millis(100))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let result = client
        .get("http://192.0.2.1:82/unreachable")
        .unwrap()
        .send()
        .await;
    assert!(result.is_err(), "connect_timeout should fire");
    let err = result.unwrap_err();
    assert!(err.is_timeout() || err.is_connect());
}

// 5b. Overall timeout does not interfere with fast successful requests.
#[tokio::test]
async fn overall_timeout_allows_fast_requests() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let (addr, _counter) = h1_server().await;
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

// 5c. read_timeout fires on stalled body reads, independently of overall timeout.
#[tokio::test]
async fn read_timeout_independent_of_overall_timeout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let read_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        // Send headers + partial body, then stall.
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        // Never send the remaining 5 bytes.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let resp = client
        .get(&format!("http://{read_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    let body_result = resp.text().await;
    assert!(
        body_result.is_err(),
        "read_timeout should fire on stalled body chunks"
    );
    assert!(
        body_result.unwrap_err().is_timeout(),
        "error should be a timeout error"
    );
}
