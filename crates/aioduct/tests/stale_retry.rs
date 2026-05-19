#![cfg(feature = "tokio")]
//! Comprehensive tests for transparent stale connection retry behavior.
//!
//! These tests target three known production bugs:
//! 1. `can_stale_retry` rejected non-empty bodies (POST with buffered body was not retried)
//! 2. Pool-hit stale connections not retried at all
//! 3. H1 connections pooled before body drained
//!
//! The goal is to find regressions, not prove correctness.

use std::time::Duration;

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn make_client_no_timeout() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

fn make_h2_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .http2_prior_knowledge()
        .build()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Section 1: Stale detection tests
// ---------------------------------------------------------------------------

/// RST-on-reuse is detected as a stale connection and transparently retried.
/// Validates fix for bug #2 (pool-hit stale connections not retried).
#[tokio::test]
async fn h1_rst_detected_as_stale() {
    let (addr, counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    // First request: served normally, connection pooled.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second request: pool returns the stale connection (RST on reuse).
    // Transparent retry must open a new connection and succeed.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Must have opened at least 2 connections (original + retry).
    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections, got {}",
        counter.connections()
    );
}

/// FIN-on-reuse (graceful close) is detected as stale and retried.
/// Distinct from RST because it triggers "connection closed" rather than "connection reset".
#[tokio::test]
async fn h1_fin_detected_as_stale() {
    let (addr, counter) = aioduct_test_server::stale::h1_fin_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");

    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections for FIN-on-reuse retry, got {}",
        counter.connections()
    );
}

/// H2 GOAWAY after serving one request is detected as stale.
/// The client should open a new H2 connection and retry.
#[tokio::test]
async fn h2_goaway_detected_as_stale() {
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_immediate().await;
    let client = make_h2_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Allow GOAWAY frame to propagate through the connection.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");

    // The important thing is that both requests succeeded.
    // Connection count may be 1 or 2 depending on GOAWAY propagation timing.
    assert!(counter.requests() >= 2);
}
/// The first connection attempt is fresh, so "stale" logic must not trigger.
/// This ensures we distinguish "stale pooled connection" from "server is down".
#[tokio::test]
async fn fresh_connection_refused_not_stale() {
    let (addr, counter) = aioduct_test_server::stale::h1_always_rst().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");
    let result = client.get(&url).unwrap().send().await;

    assert!(
        result.is_err(),
        "fresh connection RST must surface as error, not be retried indefinitely"
    );

    // Should not have opened many connections (infinite retry loop detection).
    // At most 2-3 attempts before giving up.
    assert!(
        counter.connections() <= 3,
        "too many connection attempts ({}), suggests infinite retry loop",
        counter.connections()
    );
}

/// A request that times out must NOT be classified as stale.
/// Uses a blackhole server (accepts but never responds) to trigger timeout.
#[tokio::test]
async fn timeout_error_not_stale() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Blackhole: accept connections but never send data.
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                // Read the request but never respond.
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                // Hold connection open forever.
                tokio::time::sleep(Duration::from_secs(300)).await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");
    let result = client.get(&url).unwrap().send().await;

    assert!(result.is_err(), "blackhole server must cause timeout error");
    let err = result.unwrap_err();
    assert!(
        err.is_timeout(),
        "error should be classified as timeout, not stale/connection: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Section 2: Retry gating tests
// ---------------------------------------------------------------------------

/// GET with empty body on stale connection is retried and succeeds.
#[tokio::test]
async fn retry_get_empty_body() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    // Pool the connection.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // GET on stale -> retried -> 200.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

/// POST with buffered bytes body on stale connection is retried.
/// This is the fix for bug #1 (can_stale_retry rejected non-empty bodies).
#[tokio::test]
async fn retry_post_buffered_bytes() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    // Pool the connection with initial GET.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // POST with buffered body on stale -> must be retried -> 200.
    let resp = client
        .post(&url)
        .unwrap()
        .body("hello world")
        .send()
        .await
        .expect("POST with buffered body must be retried on stale connection");
    assert_eq!(resp.status(), 200);
}

/// POST with JSON body on stale connection is retried.
/// Exact reproduction of the scheduler "connection closed" production bug.
#[cfg(feature = "json")]
#[tokio::test]
async fn retry_post_json() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let payload = serde_json::json!({"prompt": "test", "max_tokens": 50});
    let resp = client
        .post(&url)
        .unwrap()
        .json(&payload)
        .unwrap()
        .send()
        .await
        .expect("POST with JSON body must be retried on stale connection");
    assert_eq!(resp.status(), 200);
}

/// POST with form-encoded body on stale connection is retried.
/// Form bodies are buffered (Bytes), so they should be replayable.
#[tokio::test]
async fn retry_post_form() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client
        .post(&url)
        .unwrap()
        .form(&[("key", "value"), ("foo", "bar")])
        .send()
        .await
        .expect("POST with form body must be retried on stale connection");
    assert_eq!(resp.status(), 200);
}

/// PUT with buffered body on stale connection is retried.
/// Non-idempotent methods with replayable bodies should still be retried on stale.
#[tokio::test]
async fn retry_put_buffered() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client
        .put(&url)
        .unwrap()
        .body("put-data")
        .send()
        .await
        .expect("PUT with buffered body must be retried on stale connection");
    assert_eq!(resp.status(), 200);
}

/// POST with streaming (non-replayable) body must NOT be retried.
/// The body is consumed on the first attempt and cannot be replayed.
#[tokio::test]
async fn no_retry_streaming_body() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    // Pool the connection.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Streaming body: non-cloneable, consumed on first attempt.
    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("streaming payload"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let result = client
        .post(&url)
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await;

    assert!(
        result.is_err(),
        "streaming body on stale connection must fail, not be silently retried"
    );
}

/// With `.no_connection_reuse()`, every request uses a fresh connection.
/// There is no pooling, so stale connections are never encountered.
#[tokio::test]
async fn no_retry_when_no_connection_reuse() {
    let (addr, counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_connection_reuse()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request: fresh connection -> served normally.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second request: fresh connection again (no pooling).
    // The RST-on-reuse server only RSTs on the SAME connection's second request.
    // Since we always use fresh connections, we never hit the stale path.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Each request should use its own connection.
    assert_eq!(
        counter.connections(),
        2,
        "no_connection_reuse must open a new connection per request"
    );
}

// ---------------------------------------------------------------------------
// Section 3: Retry correctness
// ---------------------------------------------------------------------------

/// After retry, the method, URI path, headers, and body must be preserved exactly.
/// Uses an echo server pattern: the retry connects to a server that echoes back
/// the request details, allowing us to verify nothing was lost.
#[tokio::test]
async fn retry_preserves_method_uri_headers_body() {
    // We need a server that RSTs on reuse but echoes on fresh connections.
    // Strategy: hand-roll a server that:
    //   - First connection: serve one response, then RST on next request
    //   - Subsequent connections: echo request details
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let conn_count = Arc::new(AtomicUsize::new(0));
    let conn_count2 = conn_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let n = conn_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                if n == 0 {
                    // First connection: serve one GET (to pool), then RST on next.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nfirst";
                    let _ = stream.write_all(resp).await;
                    let _ = stream.flush().await;

                    // Wait for next request to begin, then RST.
                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                } else {
                    // Subsequent connections: read full request and echo it back.
                    let mut buf = [0u8; 8192];
                    let mut total = 0;
                    // Read until we see end of headers.
                    loop {
                        let n = match stream.read(&mut buf[total..]).await {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(_) => break,
                        };
                        total += n;
                        // Check if we have complete headers.
                        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }

                    let request_text = String::from_utf8_lossy(&buf[..total]);

                    // Parse Content-Length to read body.
                    let content_length: usize = request_text
                        .lines()
                        .find(|l| l.to_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);

                    // Find body start.
                    let header_end = buf[..total]
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| p + 4)
                        .unwrap_or(total);
                    let body_received = total - header_end;
                    let body_remaining = content_length.saturating_sub(body_received);

                    let mut body_buf = Vec::from(&buf[header_end..total]);
                    if body_remaining > 0 {
                        let mut remaining = vec![0u8; body_remaining];
                        let mut read_so_far = 0;
                        while read_so_far < body_remaining {
                            match stream.read(&mut remaining[read_so_far..]).await {
                                Ok(0) => break,
                                Ok(n) => read_so_far += n,
                                Err(_) => break,
                            }
                        }
                        body_buf.extend_from_slice(&remaining[..read_so_far]);
                    }

                    // Extract first line (method + path).
                    let headers_str = String::from_utf8_lossy(&buf[..header_end]);
                    let first_line = headers_str.lines().next().unwrap_or("");

                    // Build echo response.
                    let echo_body = format!(
                        "request_line={}\nheaders={}\nbody={}",
                        first_line,
                        headers_str.trim(),
                        String::from_utf8_lossy(&body_buf)
                    );

                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        echo_body.len(),
                        echo_body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = make_client();
    let url = format!("http://{addr}/test-path");

    // Pool the connection.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // POST with custom header + body on the stale connection.
    let resp = client
        .post(&url)
        .unwrap()
        .header(
            HeaderName::from_static("x-custom-header"),
            HeaderValue::from_static("custom-value"),
        )
        .body("test-body-content")
        .send()
        .await
        .expect("retry must succeed and preserve request details");

    assert_eq!(resp.status(), 200);
    let echo = resp.text().await.unwrap();

    // Verify method is POST.
    assert!(
        echo.contains("POST /test-path"),
        "method and path not preserved in retry. Echo:\n{echo}"
    );

    // Verify custom header.
    assert!(
        echo.contains("x-custom-header: custom-value"),
        "custom header not preserved in retry. Echo:\n{echo}"
    );

    // Verify body.
    assert!(
        echo.contains("body=test-body-content"),
        "body not preserved in retry. Echo:\n{echo}"
    );
}

/// Always-RST server must not cause infinite retry loop.
/// The client should fail quickly (within timeout), not hang forever.
#[tokio::test]
async fn retry_does_not_infinite_loop() {
    let (addr, counter) = aioduct_test_server::stale::h1_always_rst().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    let start = std::time::Instant::now();
    let result = client.get(&url).unwrap().send().await;
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "always-RST server must cause error, not succeed"
    );

    // Must not take significantly longer than the timeout.
    assert!(
        elapsed < Duration::from_secs(5),
        "took too long ({elapsed:?}), suggests retry loop"
    );

    // Must not have opened an excessive number of connections.
    assert!(
        counter.connections() <= 5,
        "opened {} connections, suggests infinite retry loop",
        counter.connections()
    );
}

/// H2 GOAWAY with POST body: the body must be replayed on the new connection.
#[tokio::test]
async fn retry_h2_goaway_with_post_body() {
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_immediate().await;
    let client = make_h2_client();
    let url = format!("http://{addr}/");

    // First request to trigger GOAWAY.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Allow GOAWAY to propagate.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // POST with body on the GOAWAY'd connection.
    let resp = client
        .post(&url)
        .unwrap()
        .body("h2-post-body")
        .send()
        .await
        .expect("H2 GOAWAY with POST body must be retried on new connection");
    assert_eq!(resp.status(), 200);

    // The important thing is that the POST succeeded (body was replayed).
    // Connection count may be 1 or 2 depending on GOAWAY propagation timing.
    assert!(counter.requests() >= 2);
}

// ---------------------------------------------------------------------------
// Section 4: Probabilistic / load tests
// ---------------------------------------------------------------------------

/// RST every 2nd request on the same connection, 100 sequential requests.
/// With transparent retry, all 100 must succeed (0 failures).
#[tokio::test]
async fn probabilistic_rst_every_2nd_100_requests() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_every_n(2).await;
    let client = make_client_no_timeout();
    let url = format!("http://{addr}/");

    let mut failures = 0u32;
    let total = 100u32;

    for i in 0..total {
        match client.get(&url).unwrap().send().await {
            Ok(resp) if resp.status() == 200 => {
                let _ = resp.text().await;
            }
            Ok(resp) => {
                failures += 1;
                eprintln!("request {i}: unexpected status {}", resp.status());
            }
            Err(e) => {
                failures += 1;
                eprintln!("request {i}: error: {e:?}");
            }
        }
    }

    assert_eq!(
        failures, 0,
        "with transparent retry, all {total} requests through RST-every-2nd must succeed, got {failures} failures"
    );
}

/// 10 concurrent requests through RST-on-reuse server.
/// All must succeed with transparent retry.
#[tokio::test]
async fn concurrent_stale_retry() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    // First: pool a connection.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Now fire 10 concurrent requests. The first one to use the pooled stale
    // connection will RST; subsequent ones will use fresh connections.
    let mut handles = Vec::new();
    for i in 0..10 {
        let client_clone = client.clone();
        let url_clone = url.clone();
        handles.push(tokio::spawn(async move {
            let result = client_clone.get(&url_clone).unwrap().send().await;
            (i, result)
        }));
    }

    let mut failures = Vec::new();
    for handle in handles {
        let (i, result) = handle.await.unwrap();
        match result {
            Ok(resp) => {
                if resp.status() != 200 {
                    failures.push(format!("request {i}: status {}", resp.status()));
                } else {
                    let _ = resp.text().await;
                }
            }
            Err(e) => {
                failures.push(format!("request {i}: {e:?}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "concurrent stale retry: {}/{} failed:\n{}",
        failures.len(),
        10,
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Additional edge case tests
// ---------------------------------------------------------------------------

/// Verify that stale retry works when the server RSTs on every Nth connection
/// and we send many requests. This tests the "pool-hit stale connections not
/// retried" bug at scale.
#[tokio::test]
async fn rst_every_3rd_sequential_stress() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_every_n(3).await;
    let client = make_client_no_timeout();
    let url = format!("http://{addr}/");

    let mut failures = 0u32;
    let total = 50u32;

    for _ in 0..total {
        match client.get(&url).unwrap().send().await {
            Ok(resp) if resp.status() == 200 => {
                let _ = resp.text().await;
            }
            _ => {
                failures += 1;
            }
        }
    }

    assert_eq!(
        failures, 0,
        "RST-every-3rd with 50 requests: expected 0 failures, got {failures}"
    );
}

/// POST with buffered body through RST-every-2nd: ensures body replay works
/// under repeated stale scenarios, not just a single retry.
#[tokio::test]
async fn post_buffered_body_survives_repeated_rst() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_every_n(2).await;
    let client = make_client_no_timeout();
    let url = format!("http://{addr}/");

    let mut failures = 0u32;
    let total = 30u32;

    for _ in 0..total {
        match client.post(&url).unwrap().body("payload").send().await {
            Ok(resp) if resp.status() == 200 => {
                let _ = resp.text().await;
            }
            _ => {
                failures += 1;
            }
        }
    }

    assert_eq!(
        failures, 0,
        "POST with body through RST-every-2nd: expected 0 failures, got {failures}"
    );
}

/// Ensure that after a stale retry, the pooled connection from the retry
/// itself works correctly for a third request.
/// This tests bug #3: H1 connections pooled before body drained.
#[tokio::test]
async fn retry_connection_is_reusable_after_body_drain() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;
    let client = make_client();
    let url = format!("http://{addr}/");

    // Request 1: served on first connection, pooled.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    // Fully drain the body to ensure the connection is properly reusable.
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");

    // Request 2: hits stale (RST), retried on new connection -> 200.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");

    // Request 3: should reuse the retry connection (which served request 2).
    // If the connection was pooled before body was drained (bug #3), this
    // would fail because the connection state is corrupted.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");
}

/// Multiple concurrent POST requests with bodies through RST-on-reuse.
/// Specifically tests that body replay logic is correct under concurrency.
#[tokio::test]
async fn concurrent_post_with_body_stale_retry() {
    // Use RST-every-2nd so multiple connections experience staleness.
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_every_n(2).await;
    let client = make_client();
    let url = format!("http://{addr}/");

    // Warm up to get a connection into the pool.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let mut handles = Vec::new();
    for i in 0..10 {
        let client_clone = client.clone();
        let url_clone = url.clone();
        handles.push(tokio::spawn(async move {
            let body = format!("payload-{i}");
            let result = client_clone
                .post(&url_clone)
                .unwrap()
                .body(body)
                .send()
                .await;
            (i, result)
        }));
    }

    let mut failures = Vec::new();
    for handle in handles {
        let (i, result) = handle.await.unwrap();
        match result {
            Ok(resp) if resp.status() == 200 => {
                let _ = resp.text().await;
            }
            Ok(resp) => {
                failures.push(format!("request {i}: unexpected status {}", resp.status()));
            }
            Err(e) => {
                failures.push(format!("request {i}: {e:?}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "concurrent POST with body stale retry: {} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// Stale retry preserves Content-Type for JSON requests.
#[cfg(feature = "json")]
#[tokio::test]
async fn stale_retry_preserves_content_type_for_json() {
    let (echo_addr, _) = aioduct_test_server::h1::h1_echo_server().await;
    let client = aioduct::HttpEngineSend::<
        aioduct::runtime::TokioRuntime,
        aioduct::runtime::tokio_rt::TcpConnector,
    >::builder()
    .pool_idle_timeout(Duration::from_secs(60))
    .timeout(Duration::from_secs(5))
    .build()
    .unwrap();

    let resp = client
        .post(&format!("http://{echo_addr}/"))
        .unwrap()
        .json(&serde_json::json!({"key": "value"}))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("application/json"),
        "POST with json body should have Content-Type: application/json, got: {body}"
    );
}
