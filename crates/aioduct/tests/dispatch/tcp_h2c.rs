use super::*;
// ── 56. Unix socket connection path (dispatch_send lines 593-634) ───────────

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_connection_path() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = std::env::temp_dir().join("aioduct_dispatch_test");
    let _ = std::fs::create_dir_all(&dir);
    let sock_path = dir.join("dispatch_test.sock");
    let _ = std::fs::remove_file(&sock_path);

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nunix socket";
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .unix_socket(&sock_path)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get("http://localhost/unix-test")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "unix socket");
}

// ── 57. Unix socket with connect timeout (dispatch_send lines 622-631) ──────

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_with_connect_timeout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = std::env::temp_dir().join("aioduct_dispatch_test");
    let _ = std::fs::create_dir_all(&dir);
    let sock_path = dir.join("dispatch_timeout.sock");
    let _ = std::fs::remove_file(&sock_path);

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nunix w/timeout";
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .unix_socket(&sock_path)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get("http://localhost/timeout-test")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "unix w/timeout");
}

// ── 58. AdaptiveH2c probe succeeds (dispatch_send lines 719-736) ────────────

#[tokio::test]
async fn adaptive_h2c_probe_succeeds_on_h2_server() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2c adaptive ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Use forward() with adaptive_h2c() to trigger the AdaptiveH2c protocol hint
    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/adaptive-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2c adaptive ok");

    // Second request uses cached probe result (should skip probe)
    let incoming2 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/adaptive-test2")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp2 = client
        .forward(super::valid_forward_request(incoming2))
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), http::StatusCode::OK);
    assert_eq!(resp2.text().await.unwrap(), "h2c adaptive ok");

    // H2 multiplex should keep connections low
    assert!(
        counter.connections() <= 2,
        "cached h2c probe should reuse connection, got {} connections",
        counter.connections()
    );
}

// ── 59. AdaptiveH2c probe falls back to H1 (lines 737-757 + 839-843) ───────

#[tokio::test]
async fn adaptive_h2c_probe_falls_back_to_h1() {
    // H1-only server — h2c preface will be rejected
    let (addr, counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h1 fallback ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/fallback-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h1 fallback ok");

    // Second request uses cached h1_only result (pool_key.protocol set to Auto)
    let incoming2 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/fallback-test2")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp2 = client
        .forward(super::valid_forward_request(incoming2))
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), http::StatusCode::OK);
    assert_eq!(resp2.text().await.unwrap(), "h1 fallback ok");

    // Probe fails on conn 1, fallback opens conn 2, second request may reuse
    assert!(
        counter.connections() >= 2,
        "adaptive h2c probe + fallback should use at least 2 connections, got {}",
        counter.connections()
    );
}

// ── 60. Connection coalescing on TLS H2 (dispatch_send lines 230-370) ───────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn connection_coalescing_reuses_h2_tls_connection() {
    use std::sync::atomic::AtomicU32;

    aioduct_test_server::tls::install_crypto_provider();

    // Generate cert with multiple SANs
    let cert =
        aioduct_test_server::tls::generate_self_signed(&["coalesce-a.local", "coalesce-b.local"]);
    let cert_der = cert.cert_der.clone();

    let counter = aioduct_test_server::ConnectionCounter::new();
    let counter2 = counter.clone();
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    // TLS H2 server with multi-SAN cert
    let config = {
        let mut cfg = rustls::ServerConfig::builder_with_provider(
            aioduct_test_server::tls::crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
        .unwrap();
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        std::sync::Arc::new(cfg)
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let acceptor = acceptor.clone();
            let req_count = request_count_clone.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = aioduct_test_server::TokioIo::new(tls_stream);
                let _ = hyper::server::conn::http2::Builder::new(aioduct_test_server::TokioExec)
                    .serve_connection(
                        io,
                        hyper::service::service_fn(move |_req| {
                            req_count.fetch_add(1, Ordering::SeqCst);
                            async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "coalesced",
                                ))))
                            }
                        }),
                    )
                    .await;
            });
        }
    });

    // Client config trusting our self-signed cert
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der.clone()).unwrap();
    let mut client_config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(root_store)
            .with_no_client_auth();
    client_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(std::sync::Arc::new(client_config));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .connection_coalescing(true)
        .resolve("coalesce-a.local", addr)
        .resolve("coalesce-b.local", addr)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request to coalesce-a.local — establishes TLS H2 connection
    let resp = client
        .get(&format!("https://coalesce-a.local:{}/first", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    let _ = resp.text().await.unwrap();

    // Second request to coalesce-b.local — coalesces onto existing connection
    let resp = client
        .get(&format!("https://coalesce-b.local:{}/second", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_2);
    assert_eq!(resp.text().await.unwrap(), "coalesced");

    // Only 1 TLS connection should have been made (coalescing reused it)
    assert_eq!(
        counter.connections(),
        1,
        "connection coalescing should reuse single TLS H2 connection, got {}",
        counter.connections()
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 2);
}

// ── 61. Connection coalescing disabled opens separate connections ────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn connection_coalescing_disabled_opens_separate() {
    aioduct_test_server::tls::install_crypto_provider();

    let cert =
        aioduct_test_server::tls::generate_self_signed(&["no-coal-a.local", "no-coal-b.local"]);
    let cert_der = cert.cert_der.clone();
    let counter = aioduct_test_server::ConnectionCounter::new();
    let counter2 = counter.clone();

    let config = {
        let mut cfg = rustls::ServerConfig::builder_with_provider(
            aioduct_test_server::tls::crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
        .unwrap();
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        std::sync::Arc::new(cfg)
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = aioduct_test_server::TokioIo::new(tls_stream);
                let _ = hyper::server::conn::http2::Builder::new(aioduct_test_server::TokioExec)
                    .serve_connection(
                        io,
                        hyper::service::service_fn(|_req| async {
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("separate"))))
                        }),
                    )
                    .await;
            });
        }
    });

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der.clone()).unwrap();
    let mut client_config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(root_store)
            .with_no_client_auth();
    client_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(std::sync::Arc::new(client_config));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .connection_coalescing(false)
        .resolve("no-coal-a.local", addr)
        .resolve("no-coal-b.local", addr)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://no-coal-a.local:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    let resp = client
        .get(&format!("https://no-coal-b.local:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "separate");

    assert_eq!(
        counter.connections(),
        2,
        "coalescing disabled should open 2 connections, got {}",
        counter.connections()
    );
}

// ── 62. H2 multiplex wait spin loop (dispatch_send lines 512-578) ───────────

#[tokio::test]
async fn h2_multiplex_wait_spin_loop_many_concurrent() {
    use std::sync::atomic::AtomicU32;

    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, counter) = h2_server_with(move |_req| {
        let count = request_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("spin wait ok"))))
        }
    })
    .await;

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
    );

    // Launch 15 concurrent requests to aggressively trigger mark_connecting_h2
    let mut handles = Vec::new();
    for i in 0..15 {
        let client = client.clone();
        let url = format!("http://{addr}/spinwait{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().h2c_prior_knowledge().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "spin wait ok");
    }

    // H2 multiplexing should keep connections minimal despite many concurrent reqs
    assert!(
        counter.connections() <= 3,
        "H2 multiplex wait should converge to few connections, got {}",
        counter.connections()
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 15);
}

// ── 63. Forward with h2c (non-adaptive) exercises force_h2c path ────────────

#[tokio::test]
async fn forward_h2c_prior_knowledge_exercises_force_h2c() {
    let (addr, _counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("forward h2c ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming = http::Request::builder()
        .method(http::Method::GET)
        .uri("/h2c-forward-test")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
        .upstream(format!("http://{addr}"))
        .h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "forward h2c ok");
}

// ── 64. H2c probe cache TTL re-probes after expiry ──────────────────────────

#[tokio::test]
async fn h2c_probe_cache_ttl_forces_re_probe() {
    let (addr, counter) = h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("re-probed"))))
    })
    .await;

    // Very short TTL forces re-probe
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .h2c_probe_ttl(Duration::from_millis(1))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let incoming1 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/ttl-probe1")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming1))
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    // Wait for TTL to expire
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Second request re-probes since TTL expired
    let incoming2 = http::Request::builder()
        .method(http::Method::GET)
        .uri("/ttl-probe2")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming2))
        .upstream(format!("http://{addr}"))
        .adaptive_h2c()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "re-probed");

    assert!(
        counter.requests() >= 2,
        "TTL expiry should cause re-probe, got {} requests",
        counter.requests()
    );
}

// ── 65. TCP fast open option exercises path (line 711-713) ──────────────────

#[tokio::test]
async fn tcp_fast_open_exercises_path() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tcp_fast_open(true)
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
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 66. Local address binding exercises connect_bound (lines 678-699) ────────

#[tokio::test]
async fn local_address_binding_exercises_connect_bound() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
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
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 67. TCP keepalive interval and retries (lines 706-710) ──────────────────

#[tokio::test]
async fn tcp_keepalive_interval_and_retries() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_keepalive_interval(Duration::from_secs(30))
        .tcp_keepalive_retries(5)
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
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── 68. Switching protocols (101) skips pool checkin (lines 164, 911-913) ───

#[tokio::test]
async fn switching_protocols_skips_pool_checkin() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            )
            .await
            .unwrap();
        stream.flush().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/ws"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);
}

// ── 69. Observer TLS events (lines 789-806) ─────────────────────────────────

#[cfg(feature = "rustls")]
#[tokio::test]
async fn observer_tls_handshake_complete_event() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let obs = TestObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .request_observer(obs.clone())
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
    let _ = resp.text().await.unwrap();

    let phases = obs.phases.lock().unwrap();
    assert!(
        phases.contains(&"TlsHandshakeComplete".to_string()),
        "should emit TlsHandshakeComplete, got: {phases:?}"
    );
    assert!(
        phases.contains(&"TcpConnected".to_string()),
        "should emit TcpConnected, got: {phases:?}"
    );
}

// ── 70. H2 redundant connection discard (lines 849-857) ─────────────────────

#[tokio::test]
async fn h2_discards_redundant_connection_on_race() {
    let (addr, counter) = h2_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(2)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("race discard"))))
    })
    .await;

    let client = Arc::new(
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
    );

    let mut handles = Vec::new();
    for i in 0..12 {
        let client = client.clone();
        let url = format!("http://{addr}/race{i}");
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().h2c_prior_knowledge().send().await
        }));
    }

    for handle in handles {
        let resp = handle.await.unwrap().unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "race discard");
    }

    assert!(
        counter.connections() <= 3,
        "H2 should discard redundant connections, got {}",
        counter.connections()
    );
    assert_eq!(counter.requests(), 12);
}

// ── 71. Rate limiter wait loop (lines 52-56) ────────────────────────────────

#[tokio::test]
async fn rate_limiter_wait_loop_exercises_sleep() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("rate ok"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .max_requests_per_sec(2)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let start = std::time::Instant::now();

    for i in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/rate{i}"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(350),
        "rate limiter should delay requests, took {:?}",
        elapsed
    );
}

// ── 72. Pool hit non-retryable streaming body uses fresh connection ──────────

#[tokio::test]
async fn pool_hit_non_retryable_streaming_body_uses_fresh_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        // RST after first response
        let raw = stream.into_std().unwrap();
        let sock = socket2::SockRef::from(&raw);
        let _ = sock.set_linger(Some(Duration::from_secs(0)));
        drop(raw);

        // Accept second connection. Non-replayable bodies should avoid the
        // stale pooled connection and start here instead of retrying later.
        if let Ok((mut s2, _)) = listener.accept().await {
            let mut buf2 = [0u8; 4096];
            let _ = s2.read(&mut buf2).await;
            s2.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nfresh")
                .await
                .unwrap();
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First GET: establish pooled connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let _ = resp.text().await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // POST with streaming body — cannot be retried after a stale write, so it
    // should skip the pooled connection and use a fresh one.
    let body_stream = futures_util::stream::once(async {
        Ok::<_, std::convert::Infallible>(hyper::body::Frame::data(Bytes::from("streaming")))
    });
    let stream_body = http_body_util::StreamBody::new(body_stream);

    let incoming = http::Request::builder()
        .method(http::Method::POST)
        .uri("/post")
        .body(stream_body)
        .unwrap();

    let resp = client
        .forward(super::valid_forward_request(incoming))
        .upstream(format!("http://{addr}"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("streaming forward should use a fresh connection");

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "fresh");
}
