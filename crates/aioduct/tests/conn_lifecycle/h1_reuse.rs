use super::*;
// Repeated body drops should not leak connections.
#[tokio::test]
async fn h1_repeated_body_drop_does_not_leak_connections() {
    let (addr, counter) = aioduct_test_server::h1::h1_large_body_server(64 * 1024).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .timeout(Duration::from_secs(5))
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
        1,
        "H1 connection should be reused after body is fully consumed"
    );
}
// pool_max_idle_per_host=1 with body consumption should still allow reuse.
#[tokio::test]
async fn h1_pool_max_idle_1_with_body_consumed_reuses() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(1)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
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
// 100 sequential H1 requests should reuse a single connection.
#[tokio::test]
async fn h1_sequential_100_downloads_one_connection() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(10)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
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
#[tokio::test]
async fn pool_max_active_per_host_caps_concurrent_fresh_dials() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let connector = SlowFirstConnector::new(Duration::from_millis(150), 16);
    let connector_ref = connector.clone();

    let client =
        HttpEngineSend::<TokioRuntime, SlowFirstConnector>::builder_with_connector(connector)
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_active_per_host(1)
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
    let url = Arc::new(format!("http://{addr}/"));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        let url = Arc::clone(&url);
        handles.push(tokio::spawn(async move {
            let resp = client.get(url.as_str()).unwrap().send().await?;
            let status = resp.status();
            let _ = resp.text().await?;
            Ok::<_, aioduct::Error>(status)
        }));
    }

    let mut successes = 0;
    let mut cap_errors = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(status) => {
                assert_eq!(status, 200);
                successes += 1;
            }
            Err(err) => {
                assert!(err.is_pool_limit(), "expected pool limit error, got: {err}");
                let limit = err.pool_limit().expect("pool limit details");
                assert_eq!(
                    limit.kind(),
                    aioduct::PoolLimitKind::MaxActivePerHost,
                    "unexpected pool limit kind"
                );
                assert_eq!(limit.limit(), Some(1));
                assert!(!err.is_connect());
                assert!(!err.is_timeout());
                cap_errors += 1;
            }
        }
    }

    assert_eq!(successes, 1, "only one fresh dial may hold the active slot");
    assert_eq!(
        cap_errors, 7,
        "the remaining concurrent dials should be capped"
    );
    assert_eq!(
        connector_ref.connections(),
        1,
        "capped requests should fail before opening TCP connections"
    );
    assert_eq!(
        counter.connections(),
        1,
        "only the reserved request should reach the server"
    );
}
