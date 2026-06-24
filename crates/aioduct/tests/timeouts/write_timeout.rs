use super::*;

// ── Write Timeout Tests ────────────────────────────────────────────────────

/// Write timeout fires when the server stops consuming the request body.
#[tokio::test]
async fn write_timeout_triggers_on_slow_server() {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Server: accept connection, read headers, then stall reading body.
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
        // Read headers only — never read the body.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Large streaming body that will stall when TCP buffers fill.
    use http_body_util::BodyExt;
    let chunk = Bytes::from(vec![b'X'; 1024]);
    let num_chunks = 500;
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
        .write_timeout(Duration::from_millis(50))
        .send()
        .await;

    assert!(result.is_err(), "write_timeout should fire during upload");
    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected a timeout error, got: {err:?}");
}

/// Write timeout does not fire when the server consumes the body quickly.
#[tokio::test]
async fn write_timeout_not_triggered_when_server_consumes_quickly() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("hello")
        .write_timeout(Duration::from_secs(1))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// Per-request write_timeout overrides the client's default.
#[tokio::test]
async fn write_timeout_per_request_overrides_client_default() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    // Client default write_timeout is 10ms — would fire on any upload that
    // experiences even minimal backpressure.
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .write_timeout(Duration::from_millis(10))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Per-request override with 1s write_timeout — a generous window.
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("test")
        .write_timeout(Duration::from_secs(1))
        .send()
        .await
        .unwrap();

    // Request succeeded — the per-request 1s write_timeout overrode the
    // client default 10ms.
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// write_timeout is not retried even for idempotent methods — a failed
/// body upload cannot be safely replayed.
#[tokio::test]
async fn write_timeout_not_retried() {
    use tokio::io::AsyncReadExt;

    let attempt = Arc::new(AtomicUsize::new(0));
    let attempt_clone = Arc::clone(&attempt);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            attempt_clone.fetch_add(1, Ordering::SeqCst);

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
            // Stall: never read the body.
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Large streaming body to trigger backpressure-based write timeout.
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
        .write_timeout(Duration::from_millis(50))
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10)),
        )
        .send()
        .await;

    assert!(result.is_err(), "write_timeout should fail");
    let err = result.unwrap_err();
    assert!(err.is_timeout());

    // Only 1 attempt — write_timeout is not retryable.
    let attempts = attempt.load(Ordering::SeqCst);
    assert_eq!(
        attempts, 1,
        "write_timeout should not be retried, got {attempts} attempts"
    );
}
