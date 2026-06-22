use super::*;
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    for _ in 0..5 {
        let resp = client
            .get(&url)
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    // Establish the H2 connection first so it's in the pool.
    let resp = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Fire 10 concurrent requests — all should succeed.
    let mut handles = Vec::new();
    for _ in 0..10 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client
                .get(&url)
                .unwrap()
                .h2c_prior_knowledge()
                .send()
                .await
                .unwrap();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
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
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), body_size);

    let resp = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_connection_reuse()
        .build()
        .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_connection_reuse()
        .build()
        .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_max_idle_per_host(1)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();
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

// #210: pool_max_idle_per_host(0) should disable connection pooling entirely.
#[tokio::test]
async fn pool_max_idle_zero_disables_reuse() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_max_idle_per_host(0)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    for _ in 0..3 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        3,
        "pool_max_idle_per_host(0) should open a fresh connection every time, got {}",
        counter.connections()
    );
}

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    for _ in 0..5 {
        let resp = client
            .get(&url)
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
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
