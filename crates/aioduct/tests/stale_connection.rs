#![cfg(feature = "tokio")]
//! Tests for transparent retry on stale pool connections.
//!
//! These reproduce the TOCTOU race where `is_ready()` passes at checkout but
//! the peer kills the connection before/during `send_request`.
//!
//! Key technique: the server serves one valid response, then on the SAME
//! connection waits for the next request to begin arriving — and RSTs. This
//! guarantees is_ready() returns true (connection is "open") while send_request
//! fails. Without transparent retry, the client surfaces a connection error.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

/// Raw HTTP/1.1 200 response with keep-alive.
fn http_200_keepalive() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok"
}

/// H1: Server serves one valid response on a keep-alive connection, then
/// when the second request arrives, RSTs the socket. The connection is still
/// "open" from the client's perspective (is_ready() → true), but send_request
/// fails because the server kills it mid-flight.
///
/// Without transparent retry: fails with Hyper(Canceled, "connection closed").
/// With transparent retry: succeeds on a fresh connection.
#[tokio::test]
async fn stale_h1_rst_on_reuse_retries_transparently() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count2 = request_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = request_count2.clone();

            tokio::spawn(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);

                if n == 0 {
                    // FIRST connection: serve one response with keep-alive,
                    // then wait for the next request to arrive and RST.
                    let mut buf = [0u8; 4096];
                    // Read first request headers.
                    let _ = stream.read(&mut buf).await;
                    // Send first response (keep-alive → connection stays pooled).
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

                    // Wait for the second request to begin arriving.
                    // Read at least 1 byte of the next request line.
                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return, // client closed first
                        Ok(_) => {}
                    }

                    // RST the connection: set SO_LINGER(0) and drop.
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                } else {
                    // SUBSEQUENT connections: serve normally.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request succeeds; connection pooled (keep-alive).
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second request: pool returns the connection (is_ready() → true because
    // the TCP connection is still open). But the server RSTs when it sees the
    // request bytes. Without transparent retry, this fails.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// Same as above but the server sends FIN (graceful close) instead of RST.
/// This produces a "connection closed" error rather than "connection reset".
#[tokio::test]
async fn stale_h1_fin_on_reuse_retries_transparently() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count2 = request_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = request_count2.clone();

            tokio::spawn(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);

                if n == 0 {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

                    // Wait for client to begin sending next request.
                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }

                    // Graceful close (FIN) instead of RST.
                    let _ = stream.shutdown().await;
                } else {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// H2 GOAWAY: server sends GOAWAY after serving one request.
/// The pooled H2 connection still shows is_ready() → true briefly, but
/// send_request fails after the GOAWAY is processed.
#[tokio::test]
async fn stale_h2_goaway_retries_transparently() {
    use hyper::server::conn::http2 as server_http2;

    #[derive(Clone)]
    struct TokioExec;
    impl<F> hyper::rt::Executor<F> for TokioExec
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            tokio::spawn(fut);
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count2 = request_count.clone();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let count = request_count2.clone();

            tokio::spawn(async move {
                count.fetch_add(1, Ordering::SeqCst);
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let svc = service_fn(|_req| async {
                    Ok::<_, std::convert::Infallible>(
                        http::Response::builder()
                            .header("content-length", "2")
                            .body(Full::new(Bytes::from("ok")))
                            .unwrap(),
                    )
                });
                let conn = server_http2::Builder::new(TokioExec).serve_connection(io, svc);
                tokio::pin!(conn);
                let _ = (&mut conn).await;
                conn.as_mut().graceful_shutdown();
                let _ = conn.await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    let resp = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Allow GOAWAY to propagate.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // With transparent retry, this succeeds on a new H2 connection.
    let resp = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// Safety: transparent retry must not loop on persistent failure.
/// A server that always RSTs should cause the request to fail, not hang.
#[tokio::test]
async fn stale_retry_does_not_loop_on_persistent_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            // RST immediately without serving anything.
            let raw = stream.into_std().unwrap();
            let sock = socket2::SockRef::from(&raw);
            let _ = sock.set_linger(Some(Duration::from_secs(0)));
            drop(raw);
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");
    let result = client.get(&url).unwrap().send().await;
    assert!(
        result.is_err(),
        "persistent failure must surface error, not hang"
    );
}

/// Body safety: requests with streaming (non-cloneable) bodies must not be
/// transparently retried. They should avoid already-pooled connections so the
/// body is not consumed by a stale write in the first place.
#[tokio::test]
async fn stale_retry_skipped_for_streaming_body() {
    use http_body_util::BodyExt;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let conn_count = Arc::new(AtomicUsize::new(0));
    let conn_count2 = conn_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let n = conn_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                if n == 0 {
                    // First connection: serve one GET, then RST on next request.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

                    // Wait for second request to begin.
                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    // RST.
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                } else {
                    // Subsequent connections: serve normally.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request (GET) succeeds; connection pooled.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // POST with streaming body. It should skip the stale pooled connection and
    // succeed on a fresh one, without attempting to replay the body.
    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("payload"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let resp = client
        .post(&url)
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .expect("streaming body should use a fresh connection instead of a stale pooled one");

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
    assert!(
        conn_count.load(Ordering::SeqCst) >= 2,
        "streaming body should skip the pooled stale connection"
    );
}

/// POST with buffered JSON body (.json()) on a stale connection.
/// Now that buffered bodies are replayed on stale connection retry,
/// this should succeed transparently — the exact fix for the scheduler
/// "connection closed" errors on POST + .json(&data).
#[cfg(feature = "json")]
#[tokio::test]
async fn stale_retry_works_for_post_json_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let conn_count = Arc::new(AtomicUsize::new(0));
    let conn_count2 = conn_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let n = conn_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                if n == 0 {
                    // First connection: serve one GET, then RST on next request.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

                    // Wait for second request to begin.
                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    // RST.
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                } else {
                    // Subsequent connections: serve normally.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request (GET) succeeds; connection pooled.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // POST with JSON body on the stale connection.
    // This is the exact pattern used by the scheduler:
    //   client.post(&url).and_then(|b| b.json(&data)).send().await
    let payload = serde_json::json!({"prompt": "hello", "max_tokens": 100});
    let result = client
        .post(&url)
        .unwrap()
        .json(&payload)
        .unwrap()
        .send()
        .await;

    // POST with buffered JSON body → replay_body available → retried → succeeds.
    let resp = result.expect("POST with buffered JSON body should be retried on stale connection");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// Contrast test: GET (empty body) on the same stale connection IS retried
/// and succeeds. This proves the retry mechanism works but is gated on body.
#[tokio::test]
async fn stale_retry_works_for_get_empty_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let conn_count = Arc::new(AtomicUsize::new(0));
    let conn_count2 = conn_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let n = conn_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                if n == 0 {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

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
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request: pooled.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second request: GET (empty body) on stale connection → retried → succeeds.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

/// #207: Middleware-applied headers must survive stale-connection retry.
///
/// A middleware adds `X-Auth: secret-token`. The server RSTs the pooled
/// connection on reuse, triggering transparent retry on a fresh connection.
/// The retry request MUST still carry the middleware-added header.
#[tokio::test]
async fn stale_retry_preserves_middleware_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let request_count = Arc::new(AtomicUsize::new(0));
    let request_count2 = request_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = request_count2.clone();

            tokio::spawn(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);

                let mut buf = [0u8; 4096];
                let bytes_read = match stream.read(&mut buf).await {
                    Ok(0) => return,
                    Ok(n) => n,
                    Err(_) => return,
                };

                if n == 0 {
                    // FIRST connection: serve one response, then RST on reuse.
                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;

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
                    // RETRY connection: check for X-Auth header.
                    let request_text = String::from_utf8_lossy(&buf[..bytes_read]);
                    let has_auth = request_text
                        .lines()
                        .any(|line| line.to_lowercase().starts_with("x-auth:"));

                    if has_auth {
                        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nauth-present";
                        let _ = stream.write_all(resp).await;
                    } else {
                        let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nauth-missing";
                        let _ = stream.write_all(resp).await;
                    }
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    "x-auth",
                    http::header::HeaderValue::from_static("secret-token"),
                );
            },
        )
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request succeeds; connection pooled.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second request: stale connection triggers retry.
    // The retry MUST carry the middleware-added X-Auth header.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "auth-present",
        "middleware-added X-Auth header was lost on stale connection retry"
    );
}

/// Probabilistic test: send many requests through a server that randomly
/// closes connections. Without transparent retry some will fail; with the
/// fix all should succeed.
#[tokio::test]
async fn stale_h1_probabilistic_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();

            tokio::spawn(async move {
                let mut served = 0u32;
                loop {
                    let mut buf = [0u8; 4096];
                    let n = match stream.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    // Check we got at least a partial HTTP request line.
                    if !buf[..n].starts_with(b"GET") && !buf[..n].starts_with(b"POST") {
                        break;
                    }

                    served += 1;

                    // After serving 1-3 requests, close the connection to simulate
                    // a server that enforces max-requests-per-connection.
                    if served >= 2 {
                        // RST to trigger the stale path.
                        let raw = stream.into_std().unwrap();
                        let sock = socket2::SockRef::from(&raw);
                        let _ = sock.set_linger(Some(Duration::from_secs(0)));
                        drop(raw);
                        return;
                    }

                    let _ = stream.write_all(http_200_keepalive()).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    let mut successes = 0u32;
    let mut failures = 0u32;
    let total = 100;

    for _ in 0..total {
        match client.get(&url).unwrap().send().await {
            Ok(resp) if resp.status() == 200 => {
                let _ = resp.text().await;
                successes += 1;
            }
            _ => {
                failures += 1;
            }
        }
    }

    // Without transparent retry, some requests will fail due to pool reuse
    // hitting the RST. With the fix, all should succeed.
    // For now (pre-fix), we just assert some failures occur to prove the
    // bug is real. After the fix, change this to assert 0 failures.
    assert_eq!(
        failures, 0,
        "expected all {total} requests to succeed with transparent retry, got {failures} failures and {successes} successes"
    );
}

/// #211: H2 multiplex-wait path must retry on stale connection errors.
///
/// Scenario: An H2 server GOAWAYs after the first request stream. Two
/// concurrent requests race — the first establishes a connection, the second
/// enters the multiplex-wait loop. When the first request completes, the
/// connection is checked into the pool. The server then sends GOAWAY. The
/// second request picks up the stale connection from the wait loop and must
/// transparently retry on a fresh connection.
///
/// Note: This test exercises the stale retry path for H2 connections broadly.
/// The multiplex-wait specific path is difficult to trigger deterministically
/// in an integration test because it requires precise timing between
/// concurrent tasks. This test validates the retry logic works for H2 GOAWAY
/// scenarios regardless of which specific code path is taken.
#[tokio::test]
async fn stale_h2_multiplex_wait_retries_on_goaway() {
    use hyper::server::conn::http2 as server_http2;

    #[derive(Clone)]
    struct TokioExec;
    impl<F> hyper::rt::Executor<F> for TokioExec
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            tokio::spawn(fut);
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let svc = service_fn(|_req| async {
                    Ok::<_, std::convert::Infallible>(
                        http::Response::builder()
                            .header("content-length", "2")
                            .body(Full::new(Bytes::from("ok")))
                            .unwrap(),
                    )
                });
                let conn = server_http2::Builder::new(TokioExec).serve_connection(io, svc);
                tokio::pin!(conn);
                let _ = (&mut conn).await;
                conn.as_mut().graceful_shutdown();
                let _ = conn.await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    let url = format!("http://{addr}/");

    // First request establishes H2 connection. Server GOAWAYs after.
    let resp = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Wait for GOAWAY to propagate and make the pooled connection stale.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second request: the pooled H2 connection is stale (GOAWAY received).
    // The stale retry must reconnect and succeed.
    let resp = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");

    // At least one retry connection must have been established, or the pool
    // correctly detected the dead connection and opened a new one. Either way,
    // the request succeeds rather than returning an error.
}
