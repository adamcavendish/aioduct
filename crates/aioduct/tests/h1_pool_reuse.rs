#![cfg(feature = "tokio")]
//! Tests for HTTP/1.1 connection pool reuse under concurrent load.
//!
//! The core issue: when an H1 connection is checked back into the pool before
//! the response body is fully consumed, the next caller sees `is_ready() == false`,
//! pops the connection from the queue (destroying it), and opens a new one.
//!
//! These tests prove that with deferred H1 check-in (waiting for the connection
//! to become ready after body drain), connections are properly reused.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::tokio_rt::{TcpConnector, TokioRuntime};

/// The key reproduction:
///
/// The server sends response headers immediately but delays the body.
/// This keeps the H1 connection in a not-ready state (body still draining)
/// when the connection is checked back into the pool.
///
/// When two requests are sent back-to-back, the second request's `checkout()`
/// finds the first connection in the pool but not-ready. It pops the
/// connection from the queue — destroying it — and opens a new one.
///
/// Without fix (current bug): every iteration destroys the connection from
/// the previous one, causing TCP connection count to grow linearly.
///
/// With deferred check-in: connections aren't placed in the pool until
/// they're actually ready, so no connection is ever destroyed by checkout.
#[tokio::test]
async fn h1_delayed_body_causes_connection_churn() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    // Server: sends headers immediately, then delays before sending the body.
    // This creates a window where is_ready() == false on the client's H1 sender.
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            accept_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if !buf[..n].starts_with(b"GET") {
                        break;
                    }

                    // Send headers immediately.
                    let headers =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\n";
                    if stream.write_all(headers).await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;

                    // Delay body — keeps the H1 connection not-ready.
                    tokio::time::sleep(Duration::from_millis(50)).await;

                    // Send body.
                    if stream.write_all(b"done").await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(10)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    let iterations = 10;
    for _ in 0..iterations {
        // Send two requests back-to-back. The second send happens while
        // the first response's body is still draining (server delays body).
        let resp1 = client.get(&url).unwrap().send().await.unwrap();
        let resp2 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), 200);
        assert_eq!(resp2.status(), 200);
        // Now consume both bodies.
        let _ = resp1.bytes().await.unwrap();
        let _ = resp2.bytes().await.unwrap();
        // Wait for connections to settle back into the pool.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let connections = accept_count.load(Ordering::SeqCst);

    // Without deferred check-in (bug):
    //   Each iteration: resp1 checks in conn (not-ready due to delayed body),
    //   resp2's checkout finds it not-ready, pops+destroys it, opens new conn.
    //   connections ≈ iterations + 1
    //
    // With deferred check-in (fix):
    //   Iteration 1: pool empty → open 2 connections
    //   Iteration 2+: both connections ready after body drain → reuse
    //   connections = 2
    assert!(
        connections <= 4, // 2 ideal + small margin
        "expected at most 4 TCP connections for {} iterations of 2 back-to-back requests \
         with delayed bodies, but server accepted {} — connections are being destroyed \
         by checkout instead of being reused",
        iterations,
        connections,
    );
}

/// Same pattern with concurrent requests across waves. The server delays
/// the response body so connections stay not-ready in the pool.
#[tokio::test]
async fn h1_concurrent_waves_with_delayed_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            accept_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if !buf[..n].starts_with(b"GET") {
                        break;
                    }

                    let headers =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\n";
                    if stream.write_all(headers).await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;

                    tokio::time::sleep(Duration::from_millis(30)).await;

                    if stream.write_all(b"done").await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(10)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    let concurrency = 4usize;
    let waves = 5usize;

    for _ in 0..waves {
        let mut handles = Vec::new();
        for _ in 0..concurrency {
            let client = client.clone();
            let url = url.clone();
            handles.push(tokio::spawn(async move {
                let resp = client.get(&url).unwrap().send().await.unwrap();
                assert_eq!(resp.status(), 200);
                let _ = resp.bytes().await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        // Wait for deferred check-in to complete.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let connections = accept_count.load(Ordering::SeqCst);

    // With fix: first wave opens `concurrency` connections, rest reuse.
    // Without fix: connections grow each wave as not-ready conns get destroyed.
    assert!(
        connections <= concurrency + 2,
        "expected at most {} connections (concurrency + margin) for {} waves, \
         but server accepted {} — connections are not being reused across waves",
        concurrency + 2,
        waves,
        connections,
    );
}

/// Baseline: sequential requests with consumed bodies should always reuse
/// a single connection, regardless of the check-in strategy.
#[tokio::test]
async fn h1_sequential_requests_reuse_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            accept_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if !buf[..n].starts_with(b"GET") {
                        break;
                    }
                    let headers =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n";
                    if stream.write_all(headers).await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    if stream.write_all(b"ok").await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    for _ in 0..10 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
        // Wait for deferred check-in.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(
        accept_count.load(Ordering::SeqCst),
        1,
        "sequential requests with consumed bodies should reuse a single connection"
    );
}
