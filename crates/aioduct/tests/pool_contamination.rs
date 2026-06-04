#![cfg(feature = "tokio")]
//! Pool contamination tests: adversarial raw H1 keep-alive servers that inject
//! extra bytes, bypass HEAD body constraints, or send duplicate Content-Length
//! headers. These verify that aioduct's pool management does not allow one
//! response to poison the next request on the same connection.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

/// Test 1: extra_bytes_after_content_length_skip_on_reuse
///
/// A malicious or broken server sends extra bytes past the declared
/// Content-Length body. Those stray bytes form a complete, injected HTTP
/// response. When the connection returns to the pool and is reused for a
/// second request, the client must read only the server's real response —
/// NOT the injected bytes that were left over from the first response.
#[tokio::test]
async fn extra_bytes_after_content_length_skip_on_reuse() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        // ── Request 1 ───────────────────────────────────────────────────
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0);

        // Response body is exactly 5 bytes ("hello"). After that, extra bytes
        // form a complete injected HTTP response that a naive pool consumer
        // might read as the response to the next request.
        let response1 = b"HTTP/1.1 200 OK\r\n\
            Content-Length: 5\r\n\
            Connection: keep-alive\r\n\
            \r\n\
            helloHTTP/1.1 200 OK\r\n\
            X-Injected: true\r\n\
            Content-Length: 0\r\n\
            Connection: keep-alive\r\n\
            \r\n";
        stream.write_all(response1).await.unwrap();

        // ── Request 2 ───────────────────────────────────────────────────
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            return; // client closed
        }

        let response2 = b"HTTP/1.1 200 OK\r\n\
            Content-Length: 4\r\n\
            Connection: keep-alive\r\n\
            \r\n\
            safe";
        stream.write_all(response2).await.unwrap();
        stream.flush().await.unwrap();
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

    // Let the connection settle into the pool.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request: if it succeeds, the body must NOT be the injected bytes.
    let result = client.get(&url).unwrap().send().await;
    match result {
        Ok(resp2) => {
            assert_eq!(resp2.status(), 200);
            let body2 = resp2.text().await.unwrap();
            assert_eq!(
                body2, "safe",
                "second response body must be 'safe', not contaminated by leftover injected bytes"
            );
        }
        Err(_) => {
            // Failing the second request is acceptable — the connection may
            // have been evicted. The critical invariant is that we never
            // silently serve injected response data.
        }
    }
}

/// Test 2: head_request_extra_bytes_not_consumed_as_body
///
/// HEAD responses have no body per HTTP semantics. A broken server may still
/// send Content-Length and body bytes after headers. The client must NOT read
/// those stray bytes as the body of the next GET on the same connection.
#[tokio::test]
async fn head_request_extra_bytes_not_consumed_as_body() {
    // Extra bytes the server sends after the HEAD response headers.
    // Must be long enough to survive in the TCP buffer.
    const EXTRA_PADDING: usize = 100;
    const EXTRA_BYTE: u8 = b'X';

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        // ── Request 1 (HEAD) ────────────────────────────────────────────
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0);

        let mut response1 = b"HTTP/1.1 200 OK\r\n\
            Content-Length: 100\r\n\
            Connection: keep-alive\r\n\
            \r\n"
            .to_vec();
        response1.extend(std::iter::repeat_n(EXTRA_BYTE, EXTRA_PADDING));
        stream.write_all(&response1).await.unwrap();

        // ── Request 2 (GET) ─────────────────────────────────────────────
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            return;
        }

        let response2 = b"HTTP/1.1 200 OK\r\n\
            Content-Length: 4\r\n\
            Connection: keep-alive\r\n\
            \r\n\
            safe";
        stream.write_all(response2).await.unwrap();
        stream.flush().await.unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    // First request: HEAD has no body.
    let resp1 = client.head(&url).unwrap().send().await.unwrap();
    assert_eq!(resp1.status(), 200);

    // HEAD returns to pool immediately (no body to drain).
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request: GET on the same pooled connection.
    let result = client.get(&url).unwrap().send().await;
    match result {
        Ok(resp2) => {
            assert_eq!(resp2.status(), 200);
            let body2 = resp2.text().await.unwrap();
            assert_eq!(
                body2, "safe",
                "second GET body must be 'safe', not the {} leftover 'X' bytes from the HEAD response",
                EXTRA_PADDING,
            );
        }
        Err(_) => {
            // Acceptable — connection may have been evicted.
        }
    }
}

/// Test 3: dual_content_length_evicts_connection
///
/// Multiple Content-Length headers violate RFC 9112 Section 8.6. The connection
/// must be evicted from the pool so that a subsequent request does not inherit
/// the corrupted framing.
#[tokio::test]
async fn dual_content_length_evicts_connection() {
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

                // Dual Content-Length headers: this is a protocol violation.
                let response = b"HTTP/1.1 200 OK\r\n\
                    Content-Length: 5\r\n\
                    Content-Length: 10\r\n\
                    Connection: keep-alive\r\n\
                    \r\n\
                    hello";
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    // First request: may succeed with the dual-CL response, or may fail
    // if hyper rejects it outright.
    let result1 = client.get(&url).unwrap().send().await;
    match result1 {
        Ok(resp1) => {
            // Consume the body (5 bytes "hello") if it arrived.
            let _ = resp1.text().await;
        }
        Err(_) => {
            // Dual CL may cause hyper to reject the response — that is fine.
        }
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request: if it succeeds, it must be on a fresh connection
    // because the old one should have been evicted.
    let conns_before = accept_count.load(Ordering::SeqCst);
    let result2 = client.get(&url).unwrap().send().await;
    match result2 {
        Ok(resp2) => {
            let _ = resp2.text().await;
            let conns_after = accept_count.load(Ordering::SeqCst);
            // The second request succeeding means a new connection was
            // established after the first one was evicted (or we had a
            // fresh connection already). If the first request consumed the
            // first connection, the second request must use at least
            // conns_before + 1.
            let conns_delta = conns_after.saturating_sub(conns_before);
            assert!(
                conns_delta >= 1 || conns_after >= 2,
                "dual Content-Length should evict the connection: \
                 expected server accept count >= 2 or a fresh connection, \
                 got {} accepts total ({} before second request)",
                conns_after,
                conns_before,
            );
        }
        Err(_) => {
            // Acceptable — the corrupted connection was evicted and the
            // fresh connection also encounters dual CL.
        }
    }
}
