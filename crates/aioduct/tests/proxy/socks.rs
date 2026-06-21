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
        .build()
        .unwrap();

    let err = client
        .get("http://localhost:80/")
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
async fn socks4_proxy_sends_userid_and_domain() {
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
            aioduct::ProxyConfig::socks4(&format!("socks4://proxyuser:ignored@{socks_addr}"))
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
