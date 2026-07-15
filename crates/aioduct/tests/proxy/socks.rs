use crate::common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_socks5_proxy() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut client, _) = socks_listener.accept().await.unwrap();

            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x05);

                client.write_all(&[0x05, 0x00]).await.unwrap();

                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);
                assert_eq!(buf[0], 0x05);
                assert_eq!(buf[1], 0x01);
                assert!(
                    buf[3] == 0x01 || buf[3] == 0x04,
                    "expected IPv4 or IPv6 ATYP, got {:#04x}",
                    buf[3]
                );

                let port = match buf[3] {
                    0x01 => u16::from_be_bytes([buf[8], buf[9]]),
                    0x04 => u16::from_be_bytes([buf[20], buf[21]]),
                    _ => unreachable!(),
                };

                let target = format!("127.0.0.1:{port}");
                let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();

                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                    .await
                    .unwrap();

                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_addr_bypasses_remote_dns_for_socks5h() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;
    let captured_target = Arc::new(std::sync::Mutex::new(None));
    let captured_target_server = captured_target.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut greeting = [0_u8; 3];
        client.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [0x05, 0x01, 0x00]);
        client.write_all(&[0x05, 0x00]).await.unwrap();

        let mut request = [0_u8; 10];
        client.read_exact(&mut request).await.unwrap();
        assert_eq!(&request[..4], &[0x05, 0x01, 0x00, 0x01]);
        let requested = SocketAddr::from((
            [request[4], request[5], request[6], request[7]],
            u16::from_be_bytes([request[8], request[9]]),
        ));
        *captured_target_server.lock().unwrap() = Some(requested);

        let mut upstream = tokio::net::TcpStream::connect(requested).await.unwrap();
        client
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5h(&format!("socks5h://{proxy_addr}")).unwrap())
        .build()
        .unwrap();
    let response = client
        .get("http://must-not-resolve.invalid:1/forced")
        .unwrap()
        .force_addr(target_addr)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert_eq!(*captured_target.lock().unwrap(), Some(target_addr));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locally_resolved_socks5_falls_back_to_next_target_address() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;
    let request_port = if target_addr.port() == u16::MAX {
        u16::MAX - 1
    } else {
        target_addr.port() + 1
    };
    let unavailable = SocketAddr::from(([127, 0, 0, 2], 9));
    let captured_targets = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_targets_server = Arc::clone(&captured_targets);
    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut client, _) = socks_listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            client.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [0x05, 0x01, 0x00]);
            client.write_all(&[0x05, 0x00]).await.unwrap();

            let mut request = [0u8; 10];
            client.read_exact(&mut request).await.unwrap();
            assert_eq!(&request[..4], &[0x05, 0x01, 0x00, 0x01]);
            let requested = SocketAddr::from((
                [request[4], request[5], request[6], request[7]],
                u16::from_be_bytes([request[8], request[9]]),
            ));
            captured_targets_server.lock().unwrap().push(requested);

            if attempt == 0 {
                client
                    .write_all(&[0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();
                continue;
            }

            let mut upstream = tokio::net::TcpStream::connect(requested).await.unwrap();
            client
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .resolve_to_addrs("socks-target.test", &[unavailable, target_addr])
        .build()
        .unwrap();

    let response = client
        .get(&format!(
            "http://socks-target.test:{}/fallback",
            request_port
        ))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert_eq!(
        captured_targets.lock().unwrap().as_slice(),
        &[unavailable, target_addr]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5_auth_negotiation_failure_does_not_retry_target_addresses() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let accepts_server = Arc::clone(&accepts);

    let server = tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        accepts_server.fetch_add(1, AtomicOrdering::SeqCst);
        let mut greeting = [0u8; 3];
        client.read_exact(&mut greeting).await.unwrap();
        client.write_all(&[0x05, 0xff]).await.unwrap();
        drop(client);

        if tokio::time::timeout(Duration::from_millis(200), socks_listener.accept())
            .await
            .is_ok()
        {
            accepts_server.fetch_add(1, AtomicOrdering::SeqCst);
        }
    });

    let first = SocketAddr::from(([127, 0, 0, 2], 8080));
    let second = SocketAddr::from(([127, 0, 0, 1], 8080));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .resolve_to_addrs("auth-target.test", &[first, second])
        .build()
        .unwrap();

    let error = client
        .get("http://auth-target.test:8080/")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no acceptable authentication"));
    server.await.unwrap();
    assert_eq!(accepts.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_socks5_proxy_with_auth() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut client, _) = socks_listener.accept().await.unwrap();

            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x05);

                client.write_all(&[0x05, 0x02]).await.unwrap();

                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x01);
                let ulen = buf[1] as usize;
                let username = String::from_utf8_lossy(&buf[2..2 + ulen]).to_string();
                let plen = buf[2 + ulen] as usize;
                let password = String::from_utf8_lossy(&buf[3 + ulen..3 + ulen + plen]).to_string();

                if username == "testuser" && password == "testpass" {
                    client.write_all(&[0x01, 0x00]).await.unwrap();
                } else {
                    client.write_all(&[0x01, 0x01]).await.unwrap();
                    return;
                }

                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);

                let port = match buf[3] {
                    0x01 => u16::from_be_bytes([buf[8], buf[9]]),
                    0x03 => {
                        let domain_len = buf[4] as usize;
                        let port_offset = 5 + domain_len;
                        u16::from_be_bytes([buf[port_offset], buf[port_offset + 1]])
                    }
                    0x04 => u16::from_be_bytes([buf[20], buf[21]]),
                    _ => panic!("unexpected ATYP: {:#04x}", buf[3]),
                };

                let target = format!("127.0.0.1:{port}");
                let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();

                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                    .await
                    .unwrap();

                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}"))
                .unwrap()
                .basic_auth("testuser", "testpass"),
        )
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_socks5h_proxy() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut client, _) = socks_listener.accept().await.unwrap();

            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x05);

                client.write_all(&[0x05, 0x00]).await.unwrap();

                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);
                assert_eq!(buf[0], 0x05);
                assert_eq!(buf[1], 0x01);
                assert_eq!(buf[3], 0x03);

                let domain_len = buf[4] as usize;
                let port_offset = 5 + domain_len;
                let port = ((buf[port_offset] as u16) << 8) | (buf[port_offset + 1] as u16);

                let target = format!("127.0.0.1:{port}");
                let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();

                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                    .await
                    .unwrap();

                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5h(&format!("socks5h://{socks_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5_auth_failure_is_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 3);
        assert_eq!(buf[0], 0x05);

        client.write_all(&[0x05, 0x02]).await.unwrap();

        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 3);
        assert_eq!(buf[0], 0x01);
        client.write_all(&[0x01, 0x01]).await.unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}"))
                .unwrap()
                .basic_auth("testuser", "wrongpass"),
        )
        .build()
        .unwrap();

    let err = client
        .get("http://localhost:80/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        format!("{err}").contains("authentication failed"),
        "expected SOCKS5 authentication failure, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5_no_acceptable_auth_methods_is_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();
    let observer = crate::ProxyPhaseObserver::default();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 3);
        assert_eq!(buf[0], 0x05);
        client.write_all(&[0x05, 0xff]).await.unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .request_observer(observer.clone())
        .build()
        .unwrap();

    let err = client
        .get("http://localhost:80/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        format!("{err}").contains("no acceptable authentication method"),
        "expected SOCKS5 method negotiation failure, got: {err}"
    );
    assert!(
        observer.phases().contains(&"tcp"),
        "a completed proxy TCP connection must be observed before SOCKS negotiation fails"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5_reply_failure_code_is_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 3);
        assert_eq!(buf[0], 0x05);
        client.write_all(&[0x05, 0x00]).await.unwrap();

        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 7);
        assert_eq!(buf[0], 0x05);
        assert_eq!(buf[1], 0x01);
        client
            .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .resolve(
            "socks-reply.test",
            std::net::SocketAddr::from(([127, 0, 0, 1], 80)),
        )
        .build()
        .unwrap();

    let err = client
        .get("http://socks-reply.test:80/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        format!("{err}").contains("connection refused"),
        "expected SOCKS5 reply failure, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5_nonzero_reply_reserved_byte_is_error() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut greeting = [0u8; 3];
        client.read_exact(&mut greeting).await.unwrap();
        client.write_all(&[0x05, 0x00]).await.unwrap();

        let mut request = [0u8; 10];
        client.read_exact(&mut request).await.unwrap();
        client
            .write_all(&[0x05, 0x00, 0x01, 0x01, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .resolve("reserved-byte.test", SocketAddr::from(([127, 0, 0, 1], 80)))
        .build()
        .unwrap();

    let error = client
        .get("http://reserved-byte.test:80/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("reserved byte 0x01"),
        "expected SOCKS5 reserved-byte failure, got: {error}"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_proxy_observer_reports_completed_phases_before_origin_tls_failure() {
    let target_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (target, _) = target_listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(target);
    });

    let (proxy_addr, proxy_cert, _connections) = tls_connect_proxy().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&proxy_cert),
    );
    let observer = crate::ProxyPhaseObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .request_observer(observer.clone())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    client
        .get(&format!("https://{target_addr}/fails-during-origin-tls"))
        .unwrap()
        .send()
        .await
        .unwrap_err();

    let phases = observer.phases();
    assert!(
        phases.contains(&"tcp"),
        "missing completed proxy TCP phase after origin TLS failure: {phases:?}"
    );
    assert!(
        phases.contains(&"tls"),
        "missing completed proxy TLS phase after origin TLS failure: {phases:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5h_preserves_remote_dns_hostname() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;
    let captured_host = Arc::new(std::sync::Mutex::new(String::new()));
    let captured_host_clone = captured_host.clone();

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut buf = [0u8; 256];
        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 3);
        assert_eq!(buf[0], 0x05);
        client.write_all(&[0x05, 0x00]).await.unwrap();

        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 7);
        assert_eq!(buf[0], 0x05);
        assert_eq!(buf[1], 0x01);
        assert_eq!(buf[3], 0x03);

        let domain_len = buf[4] as usize;
        let host = String::from_utf8_lossy(&buf[5..5 + domain_len]).to_string();
        *captured_host_clone.lock().unwrap() = host;
        let port_offset = 5 + domain_len;
        let port = u16::from_be_bytes([buf[port_offset], buf[port_offset + 1]]);

        let mut upstream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        client
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5h(&format!("socks5h://{socks_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert_eq!(captured_host.lock().unwrap().as_str(), "localhost");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5h_encodes_ip_literal_with_ip_address_type() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;
    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut greeting = [0_u8; 3];
        client.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [0x05, 0x01, 0x00]);
        client.write_all(&[0x05, 0x00]).await.unwrap();

        let mut request = [0_u8; 10];
        client.read_exact(&mut request).await.unwrap();
        assert_eq!(&request[..4], &[0x05, 0x01, 0x00, 0x01]);
        assert_eq!(&request[4..8], &[127, 0, 0, 1]);
        assert_eq!(
            u16::from_be_bytes([request[8], request[9]]),
            target_addr.port()
        );

        let mut upstream = tokio::net::TcpStream::connect(target_addr).await.unwrap();
        client
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5h(&format!("socks5h://{socks_addr}")).unwrap())
        .build()
        .unwrap();
    let response = client
        .get(&format!("http://127.0.0.1:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
}

#[cfg(feature = "rustls")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5h_https_does_not_resolve_origin_locally() {
    let (target_addr, target_cert, _counter) = aioduct_test_server::tls::tls_h2_server().await;
    let socks_addr = socks5_proxy().await;
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&resolver_calls);
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&target_cert),
    );

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(aioduct::ProxyConfig::socks5h(&format!("socks5h://{socks_addr}")).unwrap())
        .connection_coalescing(true)
        .resolver(move |_host: &str, _port: u16| {
            calls.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async { Err(io::Error::other("origin DNS must remain remote")) })
                as std::pin::Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send + 'static>>
        })
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(&format!("https://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "hello tls");
    assert_eq!(resolver_calls.load(AtomicOrdering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks4a_proxy_sends_userid_and_domain() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;
    let captured_user = Arc::new(std::sync::Mutex::new(String::new()));
    let captured_domain = Arc::new(std::sync::Mutex::new(String::new()));
    let captured_user_clone = captured_user.clone();
    let captured_domain_clone = captured_domain.clone();

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut buf = [0u8; 512];
        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 10);
        assert_eq!(buf[0], 0x04);
        assert_eq!(buf[1], 0x01);

        let port = u16::from_be_bytes([buf[2], buf[3]]);
        assert_eq!(&buf[4..8], &[0, 0, 0, 1]);

        let userid_end = 8 + buf[8..n]
            .iter()
            .position(|b| *b == 0)
            .expect("SOCKS4 userid terminator");
        let userid = String::from_utf8_lossy(&buf[8..userid_end]).to_string();
        let domain_start = userid_end + 1;
        let domain_end = domain_start
            + buf[domain_start..n]
                .iter()
                .position(|b| *b == 0)
                .expect("SOCKS4 domain terminator");
        let domain = String::from_utf8_lossy(&buf[domain_start..domain_end]).to_string();

        *captured_user_clone.lock().unwrap() = userid;
        *captured_domain_clone.lock().unwrap() = domain;

        let mut upstream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        client
            .write_all(&[0x00, 0x5a, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::socks4(&format!("socks4a://proxyuser:ignored@{socks_addr}"))
                .unwrap(),
        )
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert_eq!(captured_user.lock().unwrap().as_str(), "proxyuser");
    assert_eq!(captured_domain.lock().unwrap().as_str(), "localhost");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks4_proxy_sends_locally_resolved_ipv4_without_domain() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;
    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut header = [0u8; 8];
        client.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..2], &[0x04, 0x01]);
        assert_eq!(&header[4..8], &[127, 0, 0, 1]);
        let port = u16::from_be_bytes([header[2], header[3]]);
        assert_eq!(port, target_addr.port());

        let mut userid = Vec::new();
        loop {
            let byte = client.read_u8().await.unwrap();
            if byte == 0 {
                break;
            }
            userid.push(byte);
        }
        assert_eq!(userid, b"proxyuser");

        let mut upstream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        client
            .write_all(&[0x00, 0x5a, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::socks4(&format!("socks4://proxyuser:ignored@{socks_addr}"))
                .unwrap(),
        )
        .resolve("strict-socks4.test", target_addr)
        .build()
        .unwrap();
    let request_port = if target_addr.port() == u16::MAX {
        u16::MAX - 1
    } else {
        target_addr.port() + 1
    };

    let response = client
        .get(&format!("http://strict-socks4.test:{request_port}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5_connect_headers_error_at_send_time() {
    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();
    drop(socks_listener);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}"))
                .unwrap()
                .header(
                    http::header::HeaderName::from_static("x-token"),
                    http::HeaderValue::from_static("abc"),
                ),
        )
        .build()
        .unwrap();

    let err = client
        .get("http://localhost:80/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        format!("{err}").contains("CONNECT headers are only supported by HTTP and HTTPS proxies"),
        "expected CONNECT header rejection for SOCKS proxy, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socks5_proxy_routes_to_ipv6_target() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let target_listener = TcpListener::bind("[::1]:0").await.unwrap();
    let target_addr = target_listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = target_listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        loop {
            let n = match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if buf.len() > 8192 {
                return;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nhello aioduct")
            .await
            .ok();
    });

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();
    let target_port = target_addr.port();

    tokio::spawn(async move {
        loop {
            let (mut client, _) = match socks_listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };

            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3 && buf[0] == 0x05);
                client.write_all(&[0x05, 0x00]).await.unwrap();

                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 22);
                assert_eq!(buf[0], 0x05);
                assert_eq!(buf[1], 0x01);
                assert_eq!(buf[3], 0x04, "expected ATYP_IPV6, got {:#04x}", buf[3]);
                assert_eq!(
                    &buf[4..20],
                    &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
                    "expected ::1 IPv6 address in CONNECT"
                );
                let port = u16::from_be_bytes([buf[20], buf[21]]);
                assert_eq!(port, target_port);

                let mut upstream = tokio::net::TcpStream::connect(format!("[::1]:{target_port}"))
                    .await
                    .unwrap();

                client.write_all(&[0x05, 0x00, 0x00, 0x04]).await.unwrap();
                client.write_all(&[0u8; 16]).await.unwrap();
                client.write_all(&[0x00, 0x00]).await.unwrap();

                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://[::1]:{target_port}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
