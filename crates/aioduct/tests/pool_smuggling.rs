#![cfg(feature = "tokio")]
//! Pool contamination via response smuggling tests: adversarial raw TCP servers
//! that abuse chunked transfer-encoding trailers and `Connection: close` semantics
//! to inject unsolicited responses. These verify that aioduct's pool management
//! does not allow a smuggled response to poison the next request on the same
//! connection.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

/// Test 1: `chunked_body_trailers_then_pipelined_response_clean`
///
/// A keep-alive server sends a chunked response body with trailers after the
/// zero-length chunk, followed immediately (without waiting for the next client
/// request) by a second, complete HTTP response. This simulates a server that
/// pipelines an unsolicited response on the same connection.
///
/// The client must either:
/// - Read the pipelined response and get "SAFE" as the second response body, or
/// - Evict the connection from the pool and open a fresh connection.
#[tokio::test]
async fn chunked_body_trailers_then_pipelined_response_clean() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        for req_num in 0..2 {
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }

            if req_num == 0 {
                // Request 1: chunked body with trailers, then an unsolicited
                // pipelined HTTP response sent without waiting for the client
                // to send the next request.
                let response = concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Transfer-Encoding: chunked\r\n",
                    "Connection: keep-alive\r\n",
                    "\r\n",
                    "5\r\nhello\r\n0\r\n\r\nX-Trailer: val\r\n\r\n",
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Length: 4\r\n",
                    "\r\n",
                    "SAFE",
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            } else {
                // Request 2: if the connection is still alive (i.e., the
                // pipelined response was consumed and the client is still
                // talking), send a normal response.
                let response = concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Length: 4\r\n",
                    "Connection: keep-alive\r\n",
                    "\r\n",
                    "OKOK",
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    // First request: GET, read the 5-byte chunked body "hello".
    let resp1 = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp1.status(), 200);
    let body1 = resp1.text().await.unwrap();
    assert_eq!(
        body1, "hello",
        "first response body should be exactly 'hello' (5 bytes from the chunk)"
    );

    // Let the connection settle into the pool.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request: must NOT consume the smuggled pipelined response bytes.
    let result = client.get(&url).unwrap().send().await;
    match result {
        Ok(resp2) => {
            assert_eq!(resp2.status(), 200);
            let body2 = resp2.text().await.unwrap();
            // "SAFE" is the smuggled/pipelined response sent before the second
            // request existed — accepting it means pool contamination occurred.
            // "OKOK" means the connection was evicted and a fresh one was used.
            assert_eq!(
                body2, "OKOK",
                "second response must come from fresh connection ('OKOK'), \
                 not smuggled bytes ('SAFE' = pool contamination)"
            );
        }
        Err(_) => {
            // Failing the second request is also acceptable — the connection
            // may have been evicted.
        }
    }
}

/// Test 2: `connection_close_header_respected_not_reused`
///
/// The server sends `Connection: close` with the first response, then
/// immediately sends a second (smuggled) response "EVIL" before closing
/// the TCP connection. The `Connection: close` header signals to the client
/// that the connection must not be reused. Therefore, the smuggled "EVIL"
/// response must never be read by the client's second request.
#[tokio::test]
async fn connection_close_header_respected_not_reused() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            accept_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                if !buf[..n].starts_with(b"GET") {
                    return;
                }

                // Response with `Connection: close`, 5-byte body "hello",
                // followed immediately by a smuggled response "EVIL" before
                // closing the connection.
                let response = concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Length: 5\r\n",
                    "Connection: close\r\n",
                    "\r\n",
                    "hello",
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Length: 4\r\n",
                    "\r\n",
                    "EVIL",
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    // First request: GET, read the 5-byte body "hello".
    let resp1 = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp1.status(), 200);
    let body1 = resp1.text().await.unwrap();
    assert_eq!(
        body1, "hello",
        "first response body should be exactly 'hello' (5 bytes)"
    );

    // Let the connection settle (should be evicted due to Connection: close).
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request must NOT receive the smuggled "EVIL" response.
    let result = client.get(&url).unwrap().send().await;
    match result {
        Ok(resp2) => {
            assert_eq!(resp2.status(), 200);
            let body2 = resp2.text().await.unwrap();
            assert_ne!(
                body2, "EVIL",
                "second GET must not return the smuggled 'EVIL' response \
                 — Connection: close should have prevented reuse"
            );
            // Accept "hello" (reusing the same response on a new connection)
            // or any other non-EVIL body.
        }
        Err(_) => {
            // Acceptable — the connection may have been evicted.
        }
    }

    let conns = accept_count.load(Ordering::SeqCst);
    // We expect at least 2 connections: one for the first request (which
    // was closed by the server) and one for the second request.
    assert!(
        conns >= 2,
        "Connection: close should prevent reuse; expected >= 2 connections, got {conns}"
    );
}
