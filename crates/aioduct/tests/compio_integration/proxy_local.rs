use super::*;

// ── Proxy tests via local engine (connect_local.rs coverage) ─────────

/// Start a minimal SOCKS5 proxy server on a tokio thread. Returns the proxy's
/// listen address. The proxy connects to the target using the port from the
/// SOCKS5 CONNECT request, always connecting to 127.0.0.1.
fn start_socks5_proxy_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    // Read SOCKS5 greeting
                    let mut buf = [0u8; 256];
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 3 || buf[0] != 0x05 {
                        return;
                    }

                    // Reply: no auth required
                    client.write_all(&[0x05, 0x00]).await.unwrap();

                    // Read CONNECT request
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 7 || buf[0] != 0x05 || buf[1] != 0x01 {
                        return;
                    }

                    // Parse target address
                    let port = match buf[3] {
                        0x01 => u16::from_be_bytes([buf[8], buf[9]]),
                        0x03 => {
                            let domain_len = buf[4] as usize;
                            let port_offset = 5 + domain_len;
                            u16::from_be_bytes([buf[port_offset], buf[port_offset + 1]])
                        }
                        0x04 => u16::from_be_bytes([buf[20], buf[21]]),
                        _ => return,
                    };

                    // Connect to the actual target on localhost
                    let target = format!("127.0.0.1:{port}");
                    let mut upstream = match tokio::net::TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };

                    // Reply: success
                    client
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                        .await
                        .unwrap();

                    // Bidirectional relay
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

/// Start a minimal SOCKS5 proxy server that requires username/password auth.
fn start_socks5_auth_proxy_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let mut buf = [0u8; 256];
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 3 || buf[0] != 0x05 {
                        return;
                    }

                    // Require username/password auth (method 0x02)
                    client.write_all(&[0x05, 0x02]).await.unwrap();

                    // Read auth sub-negotiation
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 3 || buf[0] != 0x01 {
                        return;
                    }
                    let ulen = buf[1] as usize;
                    let username = String::from_utf8_lossy(&buf[2..2 + ulen]).to_string();
                    let plen = buf[2 + ulen] as usize;
                    let password =
                        String::from_utf8_lossy(&buf[3 + ulen..3 + ulen + plen]).to_string();

                    if username == "proxyuser" && password == "proxypass" {
                        client.write_all(&[0x01, 0x00]).await.unwrap(); // success
                    } else {
                        client.write_all(&[0x01, 0x01]).await.unwrap(); // failure
                        return;
                    }

                    // Read CONNECT request
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 7 {
                        return;
                    }

                    let port = match buf[3] {
                        0x01 => u16::from_be_bytes([buf[8], buf[9]]),
                        0x03 => {
                            let domain_len = buf[4] as usize;
                            let port_offset = 5 + domain_len;
                            u16::from_be_bytes([buf[port_offset], buf[port_offset + 1]])
                        }
                        0x04 => u16::from_be_bytes([buf[20], buf[21]]),
                        _ => return,
                    };

                    let target = format!("127.0.0.1:{port}");
                    let mut upstream = match tokio::net::TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };

                    client
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                        .await
                        .unwrap();

                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

/// Start a minimal SOCKS4a proxy server on a tokio thread.
fn start_socks4_proxy_tokio() -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    // SOCKS4a request:
                    // VN(1) CD(1) DSTPORT(2) DSTIP(4) USERID(variable, null-terminated) HOSTNAME(variable, null-terminated)
                    let mut buf = [0u8; 512];
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 9 || buf[0] != 0x04 || buf[1] != 0x01 {
                        return;
                    }

                    let port = ((buf[2] as u16) << 8) | (buf[3] as u16);

                    // Connect to the target on localhost
                    let target = format!("127.0.0.1:{port}");
                    let mut upstream = match tokio::net::TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => {
                            // Reply: rejected
                            client
                                .write_all(&[0x00, 0x5B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                                .await
                                .ok();
                            return;
                        }
                    };

                    // Reply: request granted (VN=0, CD=0x5A, DSTPORT=0, DSTIP=0)
                    client
                        .write_all(&[0x00, 0x5A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                        .await
                        .unwrap();

                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

/// Start an HTTP CONNECT tunnel proxy on a tokio thread.
/// For HTTPS requests, the client sends CONNECT; for plain HTTP, the proxy
/// just forwards the request.
fn start_http_proxy_tokio() -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut client, _) = match listener.accept().await {
                    Ok(c) => c,
                    Err(_) => return,
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = match client.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    let head = String::from_utf8_lossy(&buf[..n]);
                    if !head.starts_with("CONNECT ") {
                        return;
                    }
                    let target = head.split_whitespace().nth(1).unwrap_or("");
                    client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    let mut target_stream = match TcpStream::connect(target).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut target_stream).await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

#[test]
fn test_compio_socks5_proxy_local() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy via compio local engine failed: {:?}",
            result.unwrap_err()
        );
    });
}

#[test]
fn test_compio_socks5_proxy_with_auth_local() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_auth_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}"))
                    .unwrap()
                    .basic_auth("proxyuser", "proxypass"),
            )
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy with auth via compio local engine failed: {:?}",
            result.unwrap_err()
        );
    });
}

#[test]
fn test_compio_socks4_proxy_local() {
    let target_addr = start_server_tokio();
    let socks4_addr = start_socks4_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks4(&format!("socks4://{socks4_addr}")).unwrap())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS4 proxy via compio local engine failed: {:?}",
            result.unwrap_err()
        );
    });
}

#[test]
fn test_compio_http_proxy_local() {
    let target_addr = start_server_tokio();
    let http_proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::http(&format!("http://{http_proxy_addr}")).unwrap())
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{target_addr}/test-path"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("hello aioduct"),
            "expected target response, got: {body}"
        );
    });
}

#[test]
fn test_compio_http_proxy_with_auth_local() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let target_addr = start_server_tokio();
    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_seen_clone = auth_seen.clone();

    let proxy_addr = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr).unwrap();

                loop {
                    let (mut client, _) = match listener.accept().await {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let auth_seen = auth_seen_clone.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 8192];
                        let n = match client.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        let head = String::from_utf8_lossy(&buf[..n]);
                        if !head.starts_with("CONNECT ") {
                            return;
                        }
                        if head.contains("proxy-authorization:")
                            || head.contains("Proxy-Authorization:")
                        {
                            auth_seen.store(true, Ordering::SeqCst);
                        }
                        let target = head.split_whitespace().nth(1).unwrap_or("");
                        client
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .await
                            .unwrap();
                        let mut target_stream = match TcpStream::connect(target).await {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                        let _ =
                            tokio::io::copy_bidirectional(&mut client, &mut target_stream).await;
                    });
                }
            });
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::http(&format!("http://{proxy_addr}"))
                    .unwrap()
                    .basic_auth("Aladdin", "open sesame"),
            )
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{target_addr}/auth-test"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });

    // Give the proxy thread a moment to process the auth check
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        auth_seen.load(Ordering::SeqCst),
        "CONNECT request should include Proxy-Authorization header"
    );
}

#[test]
fn test_compio_socks5_proxy_with_keepalive_and_fast_open() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_keepalive_interval(Duration::from_secs(10))
            .tcp_keepalive_retries(3)
            .tcp_fast_open(true)
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy with keepalive/fast_open via compio failed: {:?}",
            result.unwrap_err()
        );
    });
}

// ── Observer notification tests for TCP connections (execute_local.rs coverage) ────

#[test]
fn test_compio_observer_tcp_connected_on_plain_http() {
    use std::sync::{Arc, Mutex};

    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let phases = Arc::new(Mutex::new(Vec::<String>::new()));
        let phases_clone = phases.clone();

        struct PhaseObs(Arc<Mutex<Vec<String>>>);
        impl aioduct::observer::RequestObserver for PhaseObs {
            fn on_event(&self, event: &aioduct::observer::RequestEvent) {
                self.0.lock().unwrap().push(format!("{:?}", event.phase));
            }
            fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
        }

        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .request_observer(PhaseObs(phases_clone))
            .no_connection_reuse()
            .build_local()
            .unwrap();

        // First request -- new connection, should fire DnsResolved and TcpConnected
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let recorded = phases.lock().unwrap();
        let joined = recorded.join("\n");

        // Verify observer received DnsResolved notification
        assert!(
            joined.contains("DnsResolved"),
            "observer should have recorded DnsResolved, got:\n{joined}"
        );

        // Verify observer received TcpConnected notification (non-TLS path, execute_local.rs ~line 581-591)
        assert!(
            joined.contains("TcpConnected"),
            "observer should have recorded TcpConnected for plain HTTP, got:\n{joined}"
        );

        // Verify PoolCheckoutComplete with Miss (new connection path)
        assert!(
            joined.contains("Miss"),
            "observer should have recorded pool Miss, got:\n{joined}"
        );

        // Verify Started, RequestSent, ResponseStarted, ResponseComplete
        assert!(
            joined.contains("Started"),
            "observer should have recorded Started, got:\n{joined}"
        );
        assert!(
            joined.contains("RequestSent"),
            "observer should have recorded RequestSent, got:\n{joined}"
        );
        assert!(
            joined.contains("ResponseStarted"),
            "observer should have recorded ResponseStarted, got:\n{joined}"
        );
        assert!(
            joined.contains("ResponseComplete"),
            "observer should have recorded ResponseComplete, got:\n{joined}"
        );
    });
}

#[test]
fn test_compio_observer_pool_hit_vs_miss() {
    use std::sync::{Arc, Mutex};

    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let phases = Arc::new(Mutex::new(Vec::<String>::new()));
        let phases_clone = phases.clone();

        struct PhaseObs(Arc<Mutex<Vec<String>>>);
        impl aioduct::observer::RequestObserver for PhaseObs {
            fn on_event(&self, event: &aioduct::observer::RequestEvent) {
                self.0.lock().unwrap().push(format!("{:?}", event.phase));
            }
            fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
        }

        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .request_observer(PhaseObs(phases_clone))
            .build_local()
            .unwrap();

        let url = format!("http://{addr}/");

        // First request: pool miss, fires TcpConnected + DnsResolved
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        // Record phases from first request
        let first_phases = {
            let recorded = phases.lock().unwrap();
            recorded.clone()
        };

        // Clear for second request
        phases.lock().unwrap().clear();

        // Second request: pool hit, should NOT fire DnsResolved/TcpConnected
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let _ = resp2.text().await.unwrap();

        let second_phases = phases.lock().unwrap().clone();
        let second_joined = second_phases.join("\n");

        // First request should have DnsResolved
        let first_joined = first_phases.join("\n");
        assert!(
            first_joined.contains("DnsResolved"),
            "first request should have DnsResolved, got:\n{first_joined}"
        );

        // Second request should have pool Hit (reuse), not DnsResolved
        assert!(
            second_joined.contains("Hit"),
            "second request should have pool Hit, got:\n{second_joined}"
        );
        assert!(
            !second_joined.contains("DnsResolved"),
            "second request should NOT have DnsResolved (pool hit), got:\n{second_joined}"
        );
    });
}

// ── TCP fast open test via direct connection ─────────────────────────

#[test]
fn test_compio_tcp_fast_open_direct() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tcp_fast_open(true)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });
}

// ── TCP keepalive through proxy connections ──────────────────────────

#[test]
fn test_compio_socks5_proxy_with_tcp_keepalive() {
    let target_addr = start_server_tokio();
    let socks5_addr = start_socks5_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks5(&format!("socks5://{socks5_addr}")).unwrap())
            .tcp_keepalive(Duration::from_secs(60))
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS5 proxy with keepalive via compio failed: {:?}",
            result.unwrap_err()
        );
    });
}

// ── Proxy connection reuse test ─────────────────────────────────────

#[test]
fn test_compio_http_proxy_connection_reuse_local() {
    let target_addr = start_server_tokio();
    let http_proxy_addr = start_http_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::http(&format!("http://{http_proxy_addr}")).unwrap())
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(5)
            .build_local()
            .unwrap();

        // First request
        let resp1 = client
            .get_local(&format!("http://{target_addr}/first"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let body1 = resp1.text().await.unwrap();
        assert!(
            body1.contains("hello aioduct"),
            "first request should succeed"
        );

        // Second request -- should reuse the connection
        let resp2 = client
            .get_local(&format!("http://{target_addr}/second"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let body2 = resp2.text().await.unwrap();
        assert!(
            body2.contains("hello aioduct"),
            "second request should also succeed"
        );
    });
}

// ── Socks4 with TCP fast open and keepalive ─────────────────────────

#[test]
fn test_compio_socks4_proxy_with_tcp_options() {
    let target_addr = start_server_tokio();
    let socks4_addr = start_socks4_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::socks4(&format!("socks4://{socks4_addr}")).unwrap())
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_fast_open(true)
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local(&format!("http://127.0.0.1:{}/", target_addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_ok(),
            "SOCKS4 proxy with tcp options via compio failed: {:?}",
            result.unwrap_err()
        );
    });
}

// #84: H2 connection multiplexing should work in local (compio) path
