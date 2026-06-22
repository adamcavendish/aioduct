use super::*;
// ── 19. H2c prior knowledge ──────────────────────────────────────────────────

#[tokio::test]
async fn h2c_prior_knowledge_works() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 ok");
}

// ── 20. No connection reuse ──────────────────────────────────────────────────

#[tokio::test]
async fn no_connection_reuse_opens_new_connections() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = h1_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_connection_reuse()
        .build()
        .unwrap();

    // Make 2 requests
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await;

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await;

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    // With no_connection_reuse, each request should open a new connection
    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections, got {}",
        counter.connections()
    );
}

// ── 21. H2 pool hit — connection reuse (lines 101-168) ─────────────────────

#[tokio::test]
async fn h2_pool_hit_reuses_connection() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 reuse"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    // First request establishes the connection
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Second request should reuse the pooled H2 connection (pool hit path)
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 reuse");

    // Only 1 TCP connection should have been made
    assert_eq!(
        counter.connections(),
        1,
        "H2 should reuse the connection (pool hit), got {} connections",
        counter.connections()
    );
    // But 2 requests were served
    assert_eq!(counter.requests(), 2);
}

// ── 22. H2 multiplex wait path (lines 512-578) ─────────────────────────────

#[tokio::test]
async fn h2_concurrent_requests_multiplex_single_connection() {
    let (addr, counter) = h2_server_with(|_req| async {
        // Small delay to ensure requests overlap
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 multiplex"))))
    })
    .await;

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .unwrap(),
    );

    // Fire multiple concurrent requests to trigger the multiplex wait path
    let mut handles = Vec::new();
    for i in 0..5 {
        let client = client.clone();
        let url = format!("http://{addr}/req{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().h2c_prior_knowledge().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "h2 multiplex");
    }

    // All requests should multiplex on 1 connection (or at most 2 if there's a race)
    assert!(
        counter.connections() <= 2,
        "H2 multiplex should use minimal connections, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 5);
}

// ── 23. Stale connection retry on pool hit (lines 169-227) ──────────────────

#[tokio::test]
async fn stale_connection_retry_on_rst() {
    // The h1_rst_on_reuse server answers the first request normally, then RSTs
    // when the client tries to reuse the connection. The retry logic should
    // open a fresh connection and succeed.
    let (addr, counter) = aioduct_test_server::stale::h1_rst_on_reuse().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request succeeds and the connection is pooled
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Small delay so the server has time to RST
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second request hits stale connection in pool, should retry on fresh connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Should have opened 2 connections (first + retry)
    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections for stale retry, got {}",
        counter.connections()
    );
}

// ── 24. Stale connection retry with FIN ─────────────────────────────────────

#[tokio::test]
async fn stale_connection_retry_on_fin() {
    let (addr, counter) = aioduct_test_server::stale::h1_fin_on_reuse().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections for stale retry on FIN, got {}",
        counter.connections()
    );
}

// ── 25. TLS connection path (lines 717-835) ─────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_connection_exercises_tls_path() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "Should negotiate h2 via ALPN"
    );
    assert!(
        resp.tls_info().is_some(),
        "TLS info should be present on the response"
    );
    assert_eq!(resp.text().await.unwrap(), "hello tls");
}

// ── 26. TLS H1 fallback path ────────────────────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_h1_connection_path() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_11,
        "Should use HTTP/1.1 when server only offers http/1.1 ALPN"
    );
    assert_eq!(resp.text().await.unwrap(), "hello tls");
}

// ── 27. TLS H2 connection reuse via pool (lines 849-861) ────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_h2_multiplex_checkin_path() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .tls(connector)
            .pool_idle_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap(),
    );

    // Concurrent requests to exercise the H2 multiplex check-in path
    let mut handles = Vec::new();
    for i in 0..4 {
        let client = client.clone();
        let url = format!("https://localhost:{}/req{i}", addr.port());
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello tls");
    }

    // H2 multiplexing should use 1-2 connections for all requests
    assert!(
        counter.connections() <= 2,
        "TLS H2 multiplex should use minimal connections, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 4);
}

// ── 28. HTTP proxy with PROXY_AUTHORIZATION (lines 863-873) ─────────────────

#[tokio::test]
async fn http_proxy_injects_proxy_authorization() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_seen_clone = auth_seen.clone();

    // Start a real HTTP target server
    let (target_addr, _counter) = aioduct_test_server::h1::h1_server().await;

    // Build a CONNECT proxy that checks Proxy-Authorization on the CONNECT request
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let auth = auth_seen_clone;

    tokio::spawn(async move {
        loop {
            let (mut client, _) = proxy_listener.accept().await.unwrap();
            let auth = auth.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = client.read(&mut buf).await.unwrap();
                let req_str = String::from_utf8_lossy(&buf[..n]);
                if !req_str.starts_with("CONNECT") {
                    return;
                }
                // Check for Proxy-Authorization in the CONNECT request
                if req_str.contains("proxy-authorization:")
                    || req_str.contains("Proxy-Authorization:")
                {
                    auth.store(true, Ordering::SeqCst);
                }
                let target = req_str.split_whitespace().nth(1).unwrap_or("");
                let _ = client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await;
                let mut upstream = match tokio::net::TcpStream::connect(target).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let proxy = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .basic_auth("user", "secret");

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // plaintext HTTP request through proxy — Proxy-Authorization is in CONNECT
    let resp = client
        .get(&format!("http://{target_addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    // Give the proxy a moment to process the auth check
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        auth_seen.load(Ordering::SeqCst),
        "CONNECT request should include Proxy-Authorization header"
    );
}

// ── 29. H2 pool hit with observer reports pool outcome ──────────────────────

#[tokio::test]
async fn h2_pool_hit_observer_reports_hit() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 observed"))))
    })
    .await;

    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .request_observer(obs.clone())
        .build()
        .unwrap();

    // First request: pool miss, establishes connection
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Second request: pool hit
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 observed");

    let phases = obs.phases.lock().unwrap();
    // The second request should get PoolCheckoutComplete (hit)
    let pool_checkout_count = phases
        .iter()
        .filter(|p| *p == "PoolCheckoutComplete")
        .count();
    assert!(
        pool_checkout_count >= 2,
        "expected at least 2 PoolCheckoutComplete events, got {pool_checkout_count}"
    );
}

// ── 30. Non-connection-reuse prevents pool checkin (lines 911-914) ───────────

#[tokio::test]
async fn no_connection_reuse_prevents_pool_checkin_h2() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 no reuse"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_connection_reuse()
        .build()
        .unwrap();

    // Make 3 sequential requests - each should open a new connection
    for i in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/req{i}"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // With no_connection_reuse + H2, every request opens a new connection
    assert_eq!(
        counter.connections(),
        3,
        "no_connection_reuse should open new connection each time, got {}",
        counter.connections()
    );
}

// ── 31. H1 pool hit path (connection reuse) ─────────────────────────────────

#[tokio::test]
async fn h1_pool_hit_reuses_connection() {
    let (addr, counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h1 reuse"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    // First request establishes the connection
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Second request should reuse the pooled H1 connection
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h1 reuse");

    // Only 1 TCP connection should have been made (pool hit)
    assert_eq!(
        counter.connections(),
        1,
        "H1 should reuse the connection via pool hit, got {} connections",
        counter.connections()
    );
    assert_eq!(counter.requests(), 2);
}

// ── 32. TLS connection no ALPN → H1 path (line 807-820) ────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_no_alpn_falls_to_h1() {
    aioduct_test_server::tls::install_crypto_provider();

    // Server with empty ALPN — no protocol negotiated
    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[]).await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello tls");
}

// ── 33. TLS sequential requests reuse H2 pool (covers pool hit on H2 TLS) ──

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_h2_sequential_reuses_connection() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let url = format!("https://localhost:{}/", addr.port());

    // Three sequential requests should all use the same connection
    for _ in 0..3 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "TLS H2 sequential requests should reuse 1 connection, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 3);
}

// ── 34. H2 GOAWAY triggers reconnect ────────────────────────────────────────

#[tokio::test]
async fn h2_goaway_triggers_fresh_connection() {
    // Server sends GOAWAY after 2 requests
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_after(2).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First 2 requests go on one connection
    for _ in 0..2 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // Give server time to process GOAWAY
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Third request should open a new connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections after GOAWAY, got {}",
        counter.connections()
    );
}

// ── 35. Proxy settings with basic auth for HTTP (lines 863-873) ─────────────

#[tokio::test]
async fn proxy_settings_injects_authorization_on_http() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_seen_clone = auth_seen.clone();

    // Start a real HTTP target server
    let (target_addr, _counter) = aioduct_test_server::h1::h1_server().await;

    // Build a CONNECT proxy that checks Proxy-Authorization on the CONNECT request
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let auth = auth_seen_clone;

    tokio::spawn(async move {
        loop {
            let (mut client, _) = proxy_listener.accept().await.unwrap();
            let auth = auth.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = client.read(&mut buf).await.unwrap();
                let req_str = String::from_utf8_lossy(&buf[..n]);
                if !req_str.starts_with("CONNECT") {
                    return;
                }
                if req_str.contains("proxy-authorization:")
                    || req_str.contains("Proxy-Authorization:")
                {
                    auth.store(true, Ordering::SeqCst);
                }
                let target = req_str.split_whitespace().nth(1).unwrap_or("");
                let _ = client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await;
                let mut upstream = match tokio::net::TcpStream::connect(target).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let proxy = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .basic_auth("admin", "password123");

    let settings = aioduct::ProxySettings::all(proxy);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/api"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    // Give the proxy a moment to process the auth check
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        auth_seen.load(Ordering::SeqCst),
        "CONNECT request should include Proxy-Authorization header"
    );
}

// ── 36. Multiple stale retries with rst_every_n ─────────────────────────────

#[tokio::test]
async fn stale_retry_rst_every_n_succeeds() {
    // Server serves 2 requests per connection, then RSTs.
    // This means: first 2 requests succeed on connection 1, then the 3rd
    // request attempts reuse and hits a stale (RST'd) connection, triggering retry.
    let (addr, counter) = aioduct_test_server::stale::h1_rst_every_n(2).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First two requests succeed on the same connection
    for i in 0..2 {
        let resp = client
            .get(&format!("http://{addr}/req{i}"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // Third request: pooled connection was RST'd, retry opens a fresh one
    let resp = client
        .get(&format!("http://{addr}/req2"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Should have at least 2 connections (original + retry after RST)
    assert!(
        counter.connections() >= 2,
        "expected at least 2 connections with RST after 2 requests, got {}",
        counter.connections()
    );
}

// ── 37. H2 pool hit after multiplex checkin ─────────────────────────────────

#[tokio::test]
async fn h2_pool_hit_after_concurrent_establishment() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = h2_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 pool hit"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    // Establish connection with first request
    let resp = client
        .get(&format!("http://{addr}/setup"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Now fire concurrent requests — they should all multiplex on the pooled connection
    let client = Arc::new(client);
    let mut handles = Vec::new();
    for i in 0..3 {
        let client = client.clone();
        let url = format!("http://{addr}/concurrent{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().h2c_prior_knowledge().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    // 1 connection total: established by first request, multiplexed for all others
    assert_eq!(
        counter.connections(),
        1,
        "all requests should multiplex on 1 connection, got {}",
        counter.connections()
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 4); // 1 setup + 3 concurrent
}
