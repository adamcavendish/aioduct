#![cfg(feature = "tokio")]
//! Tests for graceful handling of server misbehavior.
//!
//! These verify that aioduct properly reports errors (timeouts, connection
//! failures, malformed responses) without panicking, hanging, or leaking memory.

use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

// ═══════════════════════════════════════════════════════════════════════════
// 1. Malformed responses
// ═══════════════════════════════════════════════════════════════════════════

/// Server sends garbage bytes that are not valid HTTP.
/// The client must surface an error, not panic or return a "success".
#[tokio::test]
async fn invalid_http_status_line() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        b"NOT HTTP AT ALL\r\ngarbage garbage garbage\r\n\r\n".to_vec()
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(
        result.is_err(),
        "invalid HTTP status line must produce an error, got: {:?}",
        result.ok().map(|r| r.status())
    );
}

/// Server sends a valid chunked-encoding header but truncates the chunked body
/// mid-chunk (sends partial chunk size/data, then closes connection).
/// The client must surface an error when attempting to read the body.
#[tokio::test]
async fn truncated_chunked_encoding() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        // Send valid headers with chunked transfer encoding.
        let headers = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let _ = stream.write_all(headers).await;

        // Send a valid first chunk.
        let _ = stream.write_all(b"5\r\nhello\r\n").await;
        let _ = stream.flush().await;

        // Send partial second chunk header (claim 100 bytes but send nothing),
        // then abruptly close.
        let _ = stream.write_all(b"64\r\npartial").await;
        let _ = stream.flush().await;

        // Close without finishing the chunk.
        let _ = stream.shutdown().await;
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let resp = client.get(&format!("http://{addr}/")).unwrap().send().await;

    // The error might surface at send() time (if hyper detects the truncation
    // before yielding headers) or at body read time.
    match resp {
        Err(_) => {} // Error at send time is fine.
        Ok(resp) => {
            // If we got headers, reading the body must fail.
            let body_result = resp.bytes().await;
            assert!(
                body_result.is_err(),
                "truncated chunked body must produce an error on read"
            );
        }
    }
}

/// Server sends an enormous amount of header data (64KB+).
/// The client must error out rather than OOM.
#[tokio::test]
async fn headers_too_large() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        // Build a response with a massive header value (64KB of 'x').
        let mut response = Vec::with_capacity(70_000);
        response.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
        response.extend_from_slice(b"X-Huge: ");
        response.extend(vec![b'x'; 65_536]);
        response.extend_from_slice(b"\r\n\r\nbody");
        response
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    // hyper has a default max header size. If it enforces it, we get an error.
    // If not, at minimum the response should not panic or OOM.
    // We accept both an error and a successful response (if the library is lenient).
    match result {
        Err(_) => {} // Expected: hyper rejects oversized headers.
        Ok(resp) => {
            // If we got through, the library at least didn't panic.
            // Just verify we can read status.
            assert_eq!(resp.status(), 200);
        }
    }
}

/// Server sends partial headers and then stalls forever.
/// With a timeout configured, the client must fire the timeout.
#[tokio::test]
async fn response_timeout_mid_headers() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        // Send the start of an HTTP response but never finish the headers.
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n")
            .await;
        let _ = stream.flush().await;
        // Stall — never send the final \r\n to complete headers.
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(500))
        .build();

    let start = tokio::time::Instant::now();
    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "stalled headers must trigger timeout");
    let err = result.unwrap_err();
    assert!(err.is_timeout(), "expected timeout error, got: {err:?}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "timeout should fire within a reasonable time, not hang"
    );
}

/// Server sends complete headers and partial body, then stalls forever.
/// With a timeout or read_timeout, the client must surface a timeout/error.
#[tokio::test]
async fn response_timeout_mid_body() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        // Send complete headers + partial body.
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial")
            .await;
        let _ = stream.flush().await;
        // Stall — never send the remaining bytes.
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(300))
        .timeout(Duration::from_secs(3))
        .build();

    let resp = client.get(&format!("http://{addr}/")).unwrap().send().await;

    match resp {
        Err(e) => {
            // Timeout at send() level is acceptable.
            assert!(e.is_timeout(), "expected timeout error, got: {e:?}");
        }
        Ok(resp) => {
            // If headers are received, body read must fail.
            let body_result = resp.bytes().await;
            assert!(
                body_result.is_err(),
                "stalled body must produce an error (timeout or connection error)"
            );
        }
    }
}

/// Server sends headers indicating chunked encoding, sends one valid chunk,
/// then never sends the terminating zero-length chunk.
#[tokio::test]
async fn incomplete_chunked_never_terminated() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n")
            .await;
        let _ = stream.flush().await;
        // Never send "0\r\n\r\n" — just stall.
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(300))
        .timeout(Duration::from_secs(3))
        .build();

    let resp = client.get(&format!("http://{addr}/")).unwrap().send().await;

    match resp {
        Err(e) => {
            assert!(e.is_timeout(), "expected timeout, got: {e:?}");
        }
        Ok(resp) => {
            let body_result = resp.bytes().await;
            assert!(
                body_result.is_err(),
                "never-terminated chunked stream must error on body read"
            );
        }
    }
}

/// Server sends a Content-Length header claiming 100 bytes but only sends 10,
/// then closes the connection.
#[tokio::test]
async fn content_length_mismatch_short_body() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec()
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let resp = client.get(&format!("http://{addr}/")).unwrap().send().await;

    match resp {
        Err(_) => {} // Error at send time is acceptable.
        Ok(resp) => {
            let body_result = resp.bytes().await;
            assert!(
                body_result.is_err(),
                "content-length mismatch (short body) must produce an error"
            );
        }
    }
}

/// Server sends a 0-byte response (empty TCP payload, immediate close).
#[tokio::test]
async fn empty_response_immediate_close() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        // Close immediately without sending anything.
        let _ = stream.shutdown().await;
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(
        result.is_err(),
        "empty response (immediate close) must produce an error"
    );
}

/// Server sends HTTP/0.9 style response (no status line, just body).
#[tokio::test]
async fn http09_style_response() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        // No HTTP/1.x status line — just raw body content.
        b"Hello this is the body with no status line\r\n".to_vec()
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    // Modern HTTP clients should reject HTTP/0.9 responses.
    assert!(
        result.is_err(),
        "HTTP/0.9 style response (no status line) must produce an error"
    );
}

/// Server sends headers with invalid characters (null bytes in header value).
#[tokio::test]
async fn null_bytes_in_headers() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        let mut resp = Vec::new();
        resp.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
        resp.extend_from_slice(b"X-Bad: value\x00with\x00nulls\r\n");
        resp.extend_from_slice(b"Content-Length: 2\r\n\r\nok");
        resp
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    // hyper should reject headers with null bytes.
    // If it doesn't, at least verify no panic.
    match result {
        Err(_) => {} // Expected: invalid header bytes rejected.
        Ok(resp) => {
            // If somehow accepted, just verify no crash.
            let _ = resp.status();
        }
    }
}

/// Server sends a response with duplicate Content-Length headers with
/// conflicting values.
#[tokio::test]
async fn duplicate_conflicting_content_length() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 100\r\n\r\nhello".to_vec()
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    // RFC 7230 says conflicting Content-Lengths must be treated as an error.
    // hyper may reject this or accept the first value.
    match result {
        Err(_) => {} // Strict: rejected.
        Ok(resp) => {
            // Lenient: at least doesn't crash.
            assert_eq!(resp.status(), 200);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Connection errors
// ═══════════════════════════════════════════════════════════════════════════

/// Connect to a port where nothing is listening.
/// Must produce a connect error, not hang forever.
#[tokio::test]
async fn connection_refused() {
    // Bind then immediately drop to get a free port with nothing listening.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "connection to closed port must fail");
    let err = result.unwrap_err();
    assert!(
        err.is_connect(),
        "expected connect error for connection refused, got: {err:?}"
    );
}

/// Server sends valid headers then RSTs the connection mid-body.
/// The client must surface an error on body read.
#[tokio::test]
async fn connection_reset_during_body() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        // Send valid headers + partial body.
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\nstart")
            .await;
        let _ = stream.flush().await;

        // Small delay to ensure client has started reading.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // RST the connection using SO_LINGER(0).
        let raw = stream.into_std().unwrap();
        let sock = socket2::SockRef::from(&raw);
        let _ = sock.set_linger(Some(Duration::from_secs(0)));
        drop(raw);
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client.get(&format!("http://{addr}/")).unwrap().send().await;

    match resp {
        Err(_) => {} // Error at send time is acceptable.
        Ok(resp) => {
            let body_result = resp.bytes().await;
            assert!(
                body_result.is_err(),
                "connection reset mid-body must produce an error on body read"
            );
        }
    }
}

/// Blackhole server accepts TCP but never responds.
/// With a request timeout, the client must fire the timeout.
#[tokio::test]
async fn connect_timeout_fires() {
    let addr = aioduct_test_server::raw::blackhole_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(500))
        .build();

    let start = tokio::time::Instant::now();
    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "blackhole server must trigger timeout");
    let err = result.unwrap_err();
    assert!(
        err.is_timeout(),
        "expected timeout error for blackhole, got: {err:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "timeout should fire promptly"
    );
}

/// Test that connect_timeout works separately from the overall request timeout.
/// Uses a non-routable address (192.0.2.1) that will never complete TCP handshake.
#[tokio::test]
async fn connect_timeout_with_nonroutable_address() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connect_timeout(Duration::from_millis(200))
        .timeout(Duration::from_secs(30))
        .build();

    let start = tokio::time::Instant::now();
    let result = client.get("http://192.0.2.1:80/").unwrap().send().await;

    assert!(result.is_err(), "non-routable address must fail");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "connect_timeout should fire quickly, took {:?}",
        elapsed
    );
}

/// Server accepts connection but sends data very slowly (1 byte per second
/// in the status line). The overall timeout should fire.
#[tokio::test]
async fn slowloris_response() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        // Send response one byte at a time, very slowly.
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        for &byte in response.iter() {
            let _ = stream.write_all(&[byte]).await;
            let _ = stream.flush().await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(500))
        .build();

    let start = tokio::time::Instant::now();
    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    // The total response takes ~8 seconds at 200ms/byte for a ~40 byte response.
    // The 500ms timeout should fire well before that.
    assert!(result.is_err(), "slowloris response must trigger timeout");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "timeout should fire promptly"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Error classification
// ═══════════════════════════════════════════════════════════════════════════

/// Verify that connection refused errors classify as is_connect() == true.
#[tokio::test]
async fn error_is_connect_for_connection_failures() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let err = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        err.is_connect(),
        "connection refused must have is_connect() == true, got: {err:?}"
    );
    // Connection refused is NOT a timeout.
    assert!(
        !err.is_timeout(),
        "connection refused should NOT be a timeout, got: {err:?}"
    );
}

/// Verify that timeout errors classify as is_timeout() == true.
#[tokio::test]
async fn error_is_timeout_for_timeouts() {
    let addr = aioduct_test_server::raw::blackhole_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(200))
        .build();

    let err = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        err.is_timeout(),
        "timeout must have is_timeout() == true, got: {err:?}"
    );
    // Generic request timeout is not a connect error — it fires after
    // the TCP connection succeeds but before headers arrive.
    assert!(
        !err.is_connect(),
        "generic timeout should NOT be is_connect(); use connect_timeout() for that"
    );
}

/// Verify that malformed response errors are NOT classified as timeout or connect.
#[tokio::test]
async fn error_classification_for_malformed_response() {
    let addr =
        aioduct_test_server::raw::raw_server(|_req| async { b"GARBAGE\r\n\r\n".to_vec() }).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    if let Err(err) = result {
        assert!(
            !err.is_timeout(),
            "malformed response should NOT be a timeout: {err:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Multiple requests with error recovery
// ═══════════════════════════════════════════════════════════════════════════

/// After encountering a connection error, subsequent requests to valid servers
/// should still succeed (no permanent poisoning of client state).
#[tokio::test]
async fn client_recovers_after_connection_error() {
    // First: attempt a connection to a dead port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = listener.local_addr().unwrap();
    drop(listener);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(2))
        .build();

    let result = client
        .get(&format!("http://{dead_addr}/"))
        .unwrap()
        .send()
        .await;
    assert!(result.is_err(), "dead port should fail");

    // Now: attempt a connection to a valid server.
    let (live_addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let resp = client
        .get(&format!("http://{live_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

/// After encountering a timeout, subsequent requests to responsive servers
/// should still succeed.
#[tokio::test]
async fn client_recovers_after_timeout() {
    let blackhole_addr = aioduct_test_server::raw::blackhole_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(200))
        .build();

    // First: timeout against blackhole.
    let result = client
        .get(&format!("http://{blackhole_addr}/"))
        .unwrap()
        .send()
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_timeout());

    // Now: valid request with generous per-request timeout.
    let (live_addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let resp = client
        .get(&format!("http://{live_addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

/// Concurrent requests to a mix of working and broken servers.
/// Working requests must succeed regardless of the broken ones.
#[tokio::test]
async fn concurrent_requests_mixed_healthy_and_broken() {
    let (live_addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let blackhole_addr = aioduct_test_server::raw::blackhole_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(500))
        .build();

    let mut handles = Vec::new();

    // Spawn requests to live server.
    for _ in 0..5 {
        let client = client.clone();
        let url = format!("http://{live_addr}/");
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            true
        }));
    }

    // Spawn requests to blackhole (will timeout).
    for _ in 0..3 {
        let client = client.clone();
        let url = format!("http://{blackhole_addr}/");
        handles.push(tokio::spawn(async move {
            let result = client.get(&url).unwrap().send().await;
            result.is_err()
        }));
    }

    for handle in handles {
        let ok = handle.await.unwrap();
        assert!(
            ok,
            "each request should either succeed or error without panic"
        );
    }
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// Partial body with RST mid-stream should return error (curl test_05_01).
#[tokio::test]
async fn partial_body_rst_mid_stream_returns_error() {
    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        let headers = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let _ = stream.write_all(headers).await;
        for _ in 0..2 {
            let chunk = format!("{:x}\r\n{}\r\n", 1024, "x".repeat(1024));
            let _ = stream.write_all(chunk.as_bytes()).await;
            let _ = stream.flush().await;
        }
        drop(stream);
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let result = resp.bytes().await;
    assert!(
        result.is_err(),
        "Incomplete chunked transfer (missing final chunk) should return error, not truncate"
    );
}

// 0-byte body download should succeed (curl test_02_01 with data-0k).
#[tokio::test]
async fn download_zero_byte_body() {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Response;
    use std::convert::Infallible;

    let (addr, _) = aioduct_test_server::h1::h1_server_with(|_req| async {
        let resp = Response::builder()
            .header("Content-Length", "0")
            .body(Full::new(Bytes::new()))
            .unwrap();
        Ok::<_, Infallible>(resp)
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), 0, "0-byte body should be empty");
}

// Stuttered chunked download should deliver complete body (curl test_04_01).
#[tokio::test]
async fn stuttered_chunked_download_complete() {
    let (addr, _) =
        aioduct_test_server::h1::h1_slow_body_server(100, Duration::from_millis(5)).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(10))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(
        body.len(),
        1000,
        "stuttered chunked download should deliver complete body"
    );
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// BUG: error.rs:134-136 is_connect() returns true for Error::Timeout, but
// Timeout is used for both connect timeouts AND read timeouts.
// A read timeout during body streaming is NOT a connection failure.
#[tokio::test]
async fn error_timeout_should_distinguish_connect_vs_read() {
    let addr =
        aioduct_test_server::raw::raw_streaming_server(|_request_bytes, mut stream| async move {
            use tokio::io::AsyncWriteExt;
            // Send headers but then stall during body
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n")
                .await
                .unwrap();
            stream.write_all(b"partial").await.unwrap();
            stream.flush().await.unwrap();
            // Stall forever — read_timeout should fire
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
        .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(200))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    // Try to read the body — should timeout
    let err = resp.bytes().await.unwrap_err();

    // is_timeout should be true
    assert!(
        err.is_timeout(),
        "read timeout error should report is_timeout() = true, got: {err}"
    );

    // is_connect should be false — we successfully connected, just timed out reading
    assert!(
        !err.is_connect(),
        "BUG: error.rs:135 includes Error::Timeout in is_connect(). \
         A read timeout during body streaming is NOT a connection failure. \
         is_connect() should be false for read timeouts, but got true."
    );
}

// BUG: error.rs:134-136 is_connect() does not classify Error::Hyper as a connection
// failure. Many connection-level failures (TCP reset, GOAWAY) surface as Error::Hyper.
#[tokio::test]
async fn error_hyper_connection_refused_should_be_connect() {
    // Connect to a port where nothing is listening
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;

    let err = result.unwrap_err();
    assert!(
        err.is_connect(),
        "Connection refused error should report is_connect() = true. \
         Error: {err}"
    );
}
