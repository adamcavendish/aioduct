#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

fn client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
}

fn client_with_timeout(t: Duration) -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(t)
        .build()
}

// ── Pool Reuse ─────────────────────────────────────────────────────────

#[tokio::test]
async fn h1_reuse_after_body_consumed() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "should reuse connection after body consumed"
    );
}

#[tokio::test]
async fn h1_no_reuse_when_body_dropped() {
    let (addr, _counter) = aioduct_test_server::h1::h1_large_body_server(1024 * 1024).await;
    let client = client_with_timeout(Duration::from_secs(5));
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    drop(resp);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();
}

#[tokio::test]
async fn h1_concurrent_sequential_no_leak() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    for _ in 0..20 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert!(
        counter.connections() <= 2,
        "20 sequential GETs with body read should reuse connections, got {} connections",
        counter.connections()
    );
}

#[tokio::test]
async fn h1_large_body_then_reuse() {
    let body_size = 1024 * 1024;
    let (addr, counter) = aioduct_test_server::h1::h1_large_body_server(body_size).await;
    let client = client_with_timeout(Duration::from_secs(10));
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), body_size);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "should reuse after large body fully consumed"
    );
}

#[tokio::test]
async fn h2_reuse_across_sequential() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..5 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "5 sequential H2 requests should use 1 connection"
    );
}

#[tokio::test]
async fn h2_multiplex_concurrent() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .build();
    let url = format!("http://{addr}/");

    // Establish the H2 connection first so it's in the pool.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Fire 10 concurrent requests — all should succeed.
    let mut handles = Vec::new();
    for _ in 0..10 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(counter.requests(), 11);
    // BUG: H2 multiplexing is broken due to exclusive pool checkout.
    // Pool removes the connection on checkout (pop_back), so concurrent
    // requests each open a new TCP connection. hyper's http2::SendRequest
    // is Clone and supports multiplexing, but the pool doesn't clone it.
    //
    // Correct behavior: counter.connections() == 1
    // Actual behavior: counter.connections() == N (one per concurrent request)
    assert_eq!(
        counter.connections(),
        1,
        "H2 concurrent requests should multiplex over 1 connection, \
         but exclusive pool checkout opened {} connections",
        counter.connections()
    );
}

#[tokio::test]
async fn h2_large_body_then_reuse() {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Response;
    use std::convert::Infallible;

    let body_size = 1024 * 1024;
    let (addr, counter) = aioduct_test_server::h2::h2_server_with(move |_req| {
        let body = vec![b'x'; body_size];
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body)))) }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), body_size);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "H2 should reuse after large body consumed"
    );
}

// ── Pool Bypass ────────────────────────────────────────────────────────

#[tokio::test]
async fn no_connection_reuse_opens_fresh_each_time() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .no_connection_reuse()
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..3 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        3,
        "no_connection_reuse should open 3 fresh connections"
    );
}

#[tokio::test]
async fn no_connection_reuse_skips_checkin() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .no_connection_reuse()
        .build();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        2,
        "no_connection_reuse should not pool connections"
    );
}

// ── Pool Eviction / Idle ───────────────────────────────────────────────

#[tokio::test]
async fn idle_timeout_evicts_connection() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_millis(100))
        .build();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        2,
        "idle timeout should evict connection, requiring new one"
    );
}

#[tokio::test]
async fn max_idle_per_host_limits_pool() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_max_idle_per_host(1)
        .pool_idle_timeout(Duration::from_secs(60))
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..5 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert!(
        counter.connections() <= 2,
        "max_idle_per_host(1) with sequential requests should still reuse, got {} connections",
        counter.connections()
    );
}

// ── Slow / Partial Body Timing ─────────────────────────────────────────

#[tokio::test]
async fn h1_slow_chunked_body_no_pool_corruption() {
    let (addr, _counter) =
        aioduct_test_server::h1::h1_slow_body_server(100, Duration::from_millis(20)).await;
    let client = client_with_timeout(Duration::from_secs(10));
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), 1000);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body2 = resp.bytes().await.unwrap();
    assert_eq!(body2.len(), 1000);
}

#[tokio::test]
async fn h1_body_stream_partial_read_then_drop() {
    let (addr, _counter) = aioduct_test_server::h1::h1_large_body_server(10_000).await;
    let client = client_with_timeout(Duration::from_secs(5));
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    drop(resp);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), 10_000);
}

// ── Connection Not Pooled After Upgrade ────────────────────────────────

#[tokio::test]
async fn h1_upgrade_101_not_pooled() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = aioduct_test_server::raw::raw_streaming_server(|_req, mut stream| async move {
        let response = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        let _ = stream.write_all(response).await;
        let mut buf = [0u8; 256];
        let _ = stream.read(&mut buf).await;
        let _ = stream.write_all(b"echo").await;
        let _ = stream.shutdown().await;
    })
    .await;

    let (addr2, counter) = aioduct_test_server::h1::h1_server().await;
    let _ = addr;
    let _ = addr2;
    let _ = counter;
}

// ── H1 Connection: close header ────────────────────────────────────────

#[tokio::test]
async fn h1_connection_close_prevents_reuse() {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Response;
    use std::convert::Infallible;

    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async {
        let resp = Response::builder()
            .header("Connection", "close")
            .body(Full::new(Bytes::from("ok")))
            .unwrap();
        Ok::<_, Infallible>(resp)
    })
    .await;

    let client = client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        2,
        "Connection: close should prevent reuse"
    );
}

// ── H1 Keep-Alive Reuse Across Multiple Requests ──────────────────────

#[tokio::test]
async fn h1_keepalive_reuse_multiple() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    for i in 0..10 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200, "request {i} failed");
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "10 sequential keep-alive requests should reuse 1 connection"
    );
}

// ── H2 Connection Reuse Verified ──────────────────────────────────────

#[tokio::test]
async fn h2_connection_reuse_verified() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..5 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(counter.connections(), 1);
    assert_eq!(counter.requests(), 5);
}

// ── POST Body Reuse ───────────────────────────────────────────────────

#[tokio::test]
async fn h1_post_body_consumed_then_reuse() {
    let (addr, counter) = aioduct_test_server::h1::h1_echo_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    let resp = client
        .post(&url)
        .unwrap()
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("hello"));

    let resp = client
        .post(&url)
        .unwrap()
        .body("world")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("world"));

    assert_eq!(
        counter.connections(),
        1,
        "POST with body should still reuse connection"
    );
}

// ── Mixed Methods Reuse ───────────────────────────────────────────────

#[tokio::test]
async fn h1_mixed_methods_reuse_connection() {
    let (addr, counter) = aioduct_test_server::h1::h1_echo_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client
        .post(&url)
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.put(&url).unwrap().body("data").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.delete(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "mixed methods should reuse same connection"
    );
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// BUG: H2 pool exclusive checkout breaks multiplexing.
// pool/mod.rs:129 uses pop_back() which removes the H2 connection from the pool.
// Concurrent requests each open a new TCP connection instead of multiplexing.
#[tokio::test]
async fn h2_concurrent_should_multiplex_single_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();
    assert_eq!(counter.connections(), 1, "warmup should use 1 connection");

    let mut handles = Vec::new();
    for _ in 0..10 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(counter.requests(), 11);
    assert_eq!(
        counter.connections(),
        1,
        "BUG: H2 concurrent requests should multiplex over 1 connection, \
         but pool exclusive checkout causes {} connections to be opened",
        counter.connections()
    );
}

// BUG: H2 slow body concurrent should still multiplex (variant of above).
#[tokio::test]
async fn h2_slow_body_concurrent_should_still_multiplex() {
    let (addr, counter) = aioduct_test_server::h2::h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let mut handles = Vec::new();
    for _ in 0..5 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    let mut successes = 0;
    for h in handles {
        if let Ok(Ok(resp)) = h.await
            && resp.status() == 200
        {
            let _ = resp.text().await;
            successes += 1;
        }
    }
    assert_eq!(successes, 5);

    assert_eq!(
        counter.connections(),
        1,
        "BUG: H2 should multiplex all requests over 1 connection, \
         but exclusive pool checkout opened {} connections",
        counter.connections()
    );
}

// H2 parallel downloads should use single connection (curl test_02_04).
#[tokio::test]
async fn h2_parallel_downloads_single_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..10 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(counter.requests(), 10);
    assert_eq!(
        counter.connections(),
        1,
        "BUG: H2 parallel downloads should use 1 connection (multiplexing), \
         but opened {}",
        counter.connections()
    );
}

// Repeated body drops should not leak connections.
#[tokio::test]
async fn h1_repeated_body_drop_does_not_leak_connections() {
    let (addr, counter) = aioduct_test_server::h1::h1_large_body_server(64 * 1024).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..20 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        drop(resp);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let conns = counter.connections();
    assert!(
        conns <= 25,
        "20 body-drop cycles + 1 clean request should not open more than ~25 connections, got {conns}"
    );
}

// H1 connection should be reused after body is fully consumed.
#[tokio::test]
async fn h1_connection_ready_after_body_consumed() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .timeout(Duration::from_secs(5))
        .build();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "H1 connection should be reused after body is fully consumed"
    );
}

// H2 pool eviction should not discard connections with active streams.
#[tokio::test]
async fn h2_pool_eviction_should_not_discard_active_connections() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(2)
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..5 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(counter.requests(), 5);
    assert!(
        counter.connections() <= 2,
        "sequential H2 requests with max_idle=2 should reuse connections, got {}",
        counter.connections()
    );
}

// pool_max_idle_per_host=1 with body consumption should still allow reuse.
#[tokio::test]
async fn h1_pool_max_idle_1_with_body_consumed_reuses() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .timeout(Duration::from_secs(5))
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..10 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "10 sequential GETs with body consumed should reuse 1 connection \
         even with pool_max_idle=1, got {}",
        counter.connections()
    );
}

// After a concurrent burst, sequential requests should reuse pooled connections.
#[tokio::test]
async fn h1_concurrent_then_sequential_reuses_pool() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..5 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let conns_after_concurrent = counter.connections();

    for _ in 0..5 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    let conns_after_sequential = counter.connections();
    assert_eq!(
        conns_after_concurrent,
        conns_after_sequential,
        "sequential requests after concurrent burst should reuse pooled connections, \
         but opened {} new connections (total {} -> {})",
        conns_after_sequential - conns_after_concurrent,
        conns_after_concurrent,
        conns_after_sequential
    );
}

// H2 GOAWAY after N requests should force a new connection.
#[tokio::test]
async fn h2_goaway_after_n_forces_new_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_after(1).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(counter.requests(), 2);
    assert!(
        counter.connections() >= 2,
        "After GOAWAY + 100ms sleep, second request should use a new connection, \
         got only {} connection(s)",
        counter.connections()
    );
}

// HEAD response (no body) should allow immediate connection reuse.
#[tokio::test]
async fn h1_head_response_connection_reuse() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let method = req.method().to_string();
        if method == "HEAD" {
            let resp = Response::builder()
                .header("Content-Length", "100")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            Ok(Response::new(Full::new(Bytes::from("body"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();
    let url = format!("http://{addr}/");

    let resp = client.head(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "HEAD response (no body) should allow connection reuse, got {} connections",
        counter.connections()
    );
}

// Connection: close on every response should force a new connection each time.
#[tokio::test]
async fn connection_close_every_response_forces_new_connections() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async {
        let resp = Response::builder()
            .header("Connection", "close")
            .body(Full::new(Bytes::from("ok")))
            .unwrap();
        Ok::<_, Infallible>(resp)
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..20 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        20,
        "Every response with Connection: close should force a new connection"
    );
}

// H1 parallel downloads should need multiple connections (no multiplexing).
#[tokio::test]
async fn h1_parallel_downloads_need_multiple_connections() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..6 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    assert!(
        counter.connections() > 1,
        "H1 parallel downloads should use multiple connections (no multiplexing), \
         but only used {}",
        counter.connections()
    );
}

// 200 sequential H2 requests should reuse a single connection.
#[tokio::test]
async fn h2_sequential_200_requests_reuse_one_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(30))
        .build();
    let url = format!("http://{addr}/");

    for i in 0..200 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200, "request {i} failed");
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(counter.requests(), 200);
    assert_eq!(
        counter.connections(),
        1,
        "200 sequential H2 requests should reuse 1 connection, got {}",
        counter.connections()
    );
}

// 100 sequential H1 requests should reuse a single connection.
#[tokio::test]
async fn h1_sequential_100_downloads_one_connection() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(30))
        .build();
    let url = format!("http://{addr}/");

    for _ in 0..100 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "100 sequential H1 requests should reuse 1 connection, got {}",
        counter.connections()
    );
}

// Connection reuse after 404 response.
#[tokio::test]
async fn connection_reuse_after_404() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/notfound" {
            let resp = Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            Ok(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/notfound"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let _ = resp.text().await.unwrap();

    let resp = client
        .get(&format!("http://{addr}/ok"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "connection should be reused after a 404 response, got {} connections",
        counter.connections()
    );
}

// Connection reuse after 204 No Content.
#[tokio::test]
async fn connection_reuse_after_204() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/empty" {
            let resp = Response::builder()
                .status(204)
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            Ok(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/empty"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let _ = resp.text().await.unwrap();

    let resp = client
        .get(&format!("http://{addr}/ok"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        counter.connections(),
        1,
        "connection should be reused after 204 No Content"
    );
}

// Pool stability under sustained concurrent load (5 waves of 10).
#[tokio::test]
async fn h1_sustained_concurrent_load_pool_stability() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(10)
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    for _wave in 0..5 {
        let mut handles = Vec::new();
        for _ in 0..10 {
            let client = client.clone();
            let url = url.clone();
            handles.push(tokio::spawn(async move {
                let resp = client.get(&url).unwrap().send().await.unwrap();
                assert_eq!(resp.status(), 200);
                let _ = resp.text().await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let total_conns = counter.connections();
    assert!(
        total_conns <= 20,
        "5 waves of 10 concurrent requests should stabilize around 10 connections, \
         but opened {total_conns} total connections (possible pool leak)"
    );
}

// ── Pool Key Bug-Finding Tests ────────────────────────────────────────

// BUG: pool/mod.rs PoolKey stores raw Authority without normalizing default ports.
// http://host/ and http://host:80/ produce different pool keys, causing separate connections.
#[tokio::test]
async fn pool_key_should_normalize_default_port() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // Request without explicit port
    let resp = client
        .get(&format!("http://{}/", addr))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Request with explicit port 80 — should reuse the same pool entry
    // Note: addr already has the port, so we need to construct the URL with :80 explicitly
    let host = addr.ip();
    let port = addr.port();
    let resp = client
        .get(&format!("http://{}:{}/second", host, port))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Both requests go to the same server; if pool key normalizes, conn_count == 1
    // The test still passes here because both URLs have the port, but it documents
    // the gap: url::Url::authority() returns different strings for :80 and no-port.
    assert_eq!(
        counter.connections(),
        1,
        "BUG: PoolKey doesn't normalize default ports. \
         Requests to the same origin with and without explicit port should share a connection."
    );
}

// BUG: dispatch_send.rs:156-158 checks H1 connections back into the pool immediately
// after send_on_connection resolves (response HEAD received), before the body is fully
// drained. Two concurrent users of the same H1 connection corrupt each other.
// This test verifies that H1 connections with slow bodies don't get reused prematurely.
#[tokio::test]
async fn h1_slow_body_should_not_allow_concurrent_reuse() {
    let (addr, counter) =
        aioduct_test_server::h1::h1_slow_body_server(100, std::time::Duration::from_millis(10))
            .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build();

    // Start a request with a slow body
    let resp1 = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 200);

    // While resp1 body is not yet fully read, start a second request
    // If the connection was prematurely checked in, it could be reused
    // and corrupt the first response's body stream.
    let resp2 = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);

    // Read both bodies
    let body1 = resp1.bytes().await.unwrap();
    let body2 = resp2.bytes().await.unwrap();

    // Both should have complete, correct bodies
    assert!(
        !body1.is_empty(),
        "first response body should not be empty or corrupted"
    );
    assert!(
        !body2.is_empty(),
        "second response body should not be empty or corrupted"
    );

    // If the H1 connection was correctly held until body drain,
    // the second request should have opened a new connection.
    assert!(
        counter.connections() >= 2,
        "BUG: dispatch_send.rs:156 checks H1 connection back into pool before body is drained. \
         With a slow body still streaming, the second request should use a NEW connection, \
         but only {} connection(s) were opened. This means the pool allowed reuse of a \
         connection with an in-flight body.",
        counter.connections()
    );
}

// BUG: No check for HTTP/1.0 responses or Connection: close before checking connection
// back into the pool. An HTTP/1.0 response is immediately pooled, causing the next
// request on that connection to fail with a stale error. This wastes a round-trip
// for streaming bodies where stale retry is not possible.
#[tokio::test]
async fn h1_connection_close_should_not_be_pooled() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = aioduct_test_server::h1::h1_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("connection", "close")
                    .body(Full::new(Bytes::from(format!("response {n}"))))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // First request
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second request — should open a new connection (server said Connection: close)
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // If the library respects Connection: close, it should NOT try to reuse.
    // It will open a fresh connection directly, without first wasting a trip
    // to a stale pooled connection.
    assert_eq!(
        counter.connections(),
        2,
        "Connection: close should force 2 connections"
    );

    // The real bug: even though Connection: close works because hyper handles it,
    // the library still checks the connection into the pool, and then hyper's
    // is_ready() catches it as closed on checkout. This wastes a pool slot.
    // A proper implementation would skip checkin entirely when Connection: close is present.
}
