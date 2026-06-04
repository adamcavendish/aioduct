#![cfg(feature = "tokio")]
//! Tests for cancellation safety: dropping send futures and body streams
//! must evict stale connections and avoid hangs.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

/// Test: dropping a send future after it has been polled to pending must
/// evict the connection from the pool. A subsequent request must open a
/// fresh connection and succeed.
///
/// Server accepts connections with a counter:
///   connection 0 — stalls (never sends response)
///   connection 1+ — serves normally
///
/// The client polls the send future, races it against a 100ms sleep,
/// drops the future, then sends a second request that must succeed via
/// a new connection.
#[tokio::test]
async fn drop_send_future_after_poll_evicts_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let n = accept_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                // Read the HTTP request.
                let _ = stream.read(&mut buf).await;

                if n == 0 {
                    // First connection: stall — never send a response.
                    // Block on another read until the client closes.
                    let _ = stream.read(&mut [0u8; 1]).await;
                } else {
                    // Subsequent connections: serve normally.
                    let resp =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                    let _ = stream.write_all(resp).await;
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

    // Poll the send future into pending, then drop it.
    let send_fut = client.get(&url).unwrap().send();
    tokio::pin!(send_fut);
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(100)) => {
            // send_fut was polled into pending via the select!, now drop it.
            // Dropping the Pin<&mut> wrapper; the underlying future (owned by
            // the pin stack slot) is dropped when it goes out of scope below.
            drop(send_fut);
        }
        _result = &mut send_fut => {
            panic!("should not complete before 100ms");
        }
    }

    // Second request must succeed on a fresh connection.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Server must have accepted at least 2 connections:
    // one stalled (evicted), one fresh (successful).
    let count = accept_count.load(Ordering::SeqCst);
    assert!(
        count >= 2,
        "expected >= 2 accepted connections, got {count}"
    );
}

/// Test: dropping a body stream after reading one chunk must not cause
/// a hang. The dropped-body connection must be evicted so a subsequent
/// request succeeds.
///
/// The server sends a chunked response with 2 chunks, then stalls (never
/// sends the terminating zero-length chunk). The client reads one chunk,
/// drops the stream, and verifies:
///   - No hang (test completes within 2 seconds).
///   - A subsequent request succeeds (fresh connection).
#[tokio::test]
async fn drop_body_stream_after_one_chunk_no_hang() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let n = accept_count2.fetch_add(1, Ordering::SeqCst);

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                // Read the HTTP request.
                let _ = stream.read(&mut buf).await;

                if n == 0 {
                    // First connection: send headers + 2 chunked body chunks,
                    // then stall (never send the terminating chunk).
                    let resp = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nwiki\r\n5\r\npedia\r\n";
                    let _ = stream.write_all(resp).await;
                    let _ = stream.flush().await;

                    // Stall: block on read forever (or until client closes).
                    let _ = stream.read(&mut [0u8; 1]).await;
                } else {
                    // Subsequent connections: serve a complete response.
                    let resp =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                    let _ = stream.write_all(resp).await;
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

    // Wrap in a timeout: the entire operation must not hang.
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        // Send GET, get the response.
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);

        // Get the body stream.
        let mut stream = resp.into_bytes_stream();

        // Read one chunk.
        let chunk = stream.next().await;
        assert!(chunk.is_some(), "expected at least one chunk");
        let chunk_data = chunk.unwrap().unwrap();
        assert_eq!(&chunk_data[..], b"wiki", "expected first chunk 'wiki'");

        // Drop the stream mid-body.
        drop(stream);

        // Verify a subsequent request succeeds.
        let resp2 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), 200);
        assert_eq!(resp2.text().await.unwrap(), "ok");
    })
    .await;

    assert!(
        result.is_ok(),
        "test hung: dropping the body stream after one chunk must not deadlock"
    );

    // Server must have accepted at least 2 connections.
    let count = accept_count.load(Ordering::SeqCst);
    assert!(
        count >= 2,
        "expected >= 2 accepted connections, got {count}"
    );
}
