use super::*;

// Concurrent H2 requests must multiplex over a single TCP connection
// rather than opening a new connection for each concurrent request.
#[tokio::test]
async fn h2_concurrent_should_multiplex_single_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
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
    let _ = resp.text().await.unwrap();
    assert_eq!(counter.connections(), 1, "warmup should use 1 connection");

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
    assert_eq!(
        counter.connections(),
        1,
        "H2 concurrent requests must multiplex over 1 connection, \
         got {} connections",
        counter.connections()
    );
}
// Concurrent H2 requests with slow bodies must still multiplex
// over a single connection (variant of above).
#[tokio::test]
async fn h2_slow_body_concurrent_should_still_multiplex() {
    let (addr, counter) = aioduct_test_server::h2::h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
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
    let _ = resp.text().await.unwrap();

    let mut handles = Vec::new();
    for _ in 0..5 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().h2c_prior_knowledge().send().await
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
        "H2 must multiplex all requests over 1 connection, \
         got {} connections",
        counter.connections()
    );
}
// H2 parallel downloads should use single connection (curl test_02_04).
#[tokio::test]
async fn h2_parallel_downloads_single_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

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

    assert_eq!(counter.requests(), 10);
    assert_eq!(
        counter.connections(),
        1,
        "BUG: H2 parallel downloads should use 1 connection (multiplexing), \
         but opened {}",
        counter.connections()
    );
}
// H2 pool eviction should not discard connections with active streams.
#[tokio::test]
async fn h2_pool_eviction_should_not_discard_active_connections() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(2)
        .timeout(Duration::from_secs(10))
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

    assert_eq!(counter.requests(), 5);
    assert!(
        counter.connections() <= 2,
        "sequential H2 requests with max_idle=2 should reuse connections, got {}",
        counter.connections()
    );
}
// H2 GOAWAY after N requests should force a new connection.
#[tokio::test]
async fn h2_goaway_after_n_forces_new_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_after(1).await;
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
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let resp = client
        .get(&url)
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
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
// 200 sequential H2 requests should reuse a single connection.
#[tokio::test]
async fn h2_sequential_200_requests_reuse_one_connection() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let url = format!("http://{addr}/");

    for i in 0..200 {
        let resp = client
            .get(&url)
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
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
