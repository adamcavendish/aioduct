use super::*;

#[cfg(feature = "rustls")]
#[path = "proxy_local/incoming_multipart.rs"]
mod incoming_multipart;
#[cfg(feature = "rustls")]
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

                    // Read SOCKS5 greeting.
                    let mut greeting = [0u8; 2];
                    if client.read_exact(&mut greeting).await.is_err() || greeting[0] != 0x05 {
                        return;
                    }
                    let mut methods = vec![0u8; greeting[1] as usize];
                    if client.read_exact(&mut methods).await.is_err() {
                        return;
                    }

                    // Reply: no auth required
                    client.write_all(&[0x05, 0x00]).await.unwrap();

                    // Read CONNECT request.
                    let mut request = [0u8; 4];
                    if client.read_exact(&mut request).await.is_err()
                        || request[..3] != [0x05, 0x01, 0x00]
                    {
                        return;
                    }

                    // Consume the target address. Locally resolved requests use
                    // their concrete IP; remote names retain the helper's
                    // localhost mapping.
                    let target_ip = match request[3] {
                        0x01 => {
                            let mut address = [0u8; 4];
                            if client.read_exact(&mut address).await.is_err() {
                                return;
                            }
                            Some(std::net::IpAddr::V4(std::net::Ipv4Addr::from(address)))
                        }
                        0x03 => {
                            let Ok(length) = client.read_u8().await else {
                                return;
                            };
                            let mut address = vec![0u8; length as usize];
                            if client.read_exact(&mut address).await.is_err() {
                                return;
                            }
                            None
                        }
                        0x04 => {
                            let mut address = [0u8; 16];
                            if client.read_exact(&mut address).await.is_err() {
                                return;
                            }
                            Some(std::net::IpAddr::V6(std::net::Ipv6Addr::from(address)))
                        }
                        _ => return,
                    };
                    let Ok(port) = client.read_u16().await else {
                        return;
                    };

                    let target = std::net::SocketAddr::new(
                        target_ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
                        port,
                    );
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

fn start_forced_socks4_proxy_tokio() -> (
    std::net::SocketAddr,
    std::sync::mpsc::Receiver<std::net::SocketAddr>,
) {
    let (address_tx, address_rx) = std::sync::mpsc::channel();
    let (target_tx, target_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            address_tx.send(listener.local_addr().unwrap()).unwrap();
            let (mut downstream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8];
            downstream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request[..2], &[0x04, 0x01]);
            let requested = std::net::SocketAddr::from((
                [request[4], request[5], request[6], request[7]],
                u16::from_be_bytes([request[2], request[3]]),
            ));
            loop {
                if downstream.read_u8().await.unwrap() == 0 {
                    break;
                }
            }
            target_tx.send(requested).unwrap();

            let mut upstream = tokio::net::TcpStream::connect(requested).await.unwrap();
            downstream
                .write_all(&[0x00, 0x5a, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
        });
    });
    (address_rx.recv().unwrap(), target_rx)
}

/// Start an HTTP CONNECT tunnel proxy on a tokio thread.
/// For HTTPS requests, the client sends CONNECT; for plain HTTP, the proxy
/// just forwards the request.
fn start_http_proxy_tokio() -> std::net::SocketAddr {
    start_http_proxy_tokio_with_version_and_count("HTTP/1.1").0
}

fn start_http10_proxy_tokio() -> std::net::SocketAddr {
    start_http_proxy_tokio_with_version_and_count("HTTP/1.0").0
}

fn start_counting_http_proxy_tokio() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    start_http_proxy_tokio_with_version_and_count("HTTP/1.1")
}

fn start_http_proxy_tokio_with_version_and_count(
    response_version: &'static str,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let (tx, rx) = std::sync::mpsc::channel();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_connections = connections.clone();
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
                server_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                        .write_all(
                            format!("{response_version} 200 Connection Established\r\n\r\n")
                                .as_bytes(),
                        )
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
    (rx.recv().unwrap(), connections)
}

#[cfg(feature = "rustls")]
fn start_https_proxy_tokio() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    start_https_proxy_tokio_with_alpn(true)
}

#[cfg(feature = "rustls")]
fn start_https_proxy_tokio_without_alpn() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    start_https_proxy_tokio_with_alpn(false)
}

#[cfg(feature = "rustls")]
fn start_https_proxy_tokio_with_alpn(
    advertise_http1_alpn: bool,
) -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    aioduct_test_server::tls::install_crypto_provider();
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    if advertise_http1_alpn {
        server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("localhost:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();

            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut client) = acceptor.accept(tcp).await else {
                        return;
                    };
                    if advertise_http1_alpn
                        && client.get_ref().1.alpn_protocol() != Some(b"http/1.1")
                    {
                        return;
                    }

                    let mut request = Vec::new();
                    let mut chunk = [0u8; 512];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let Ok(n) = client.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 || request.len() + n > 8192 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&request);
                    if !head.starts_with("CONNECT ") {
                        return;
                    }
                    let Some(target) = head.split_whitespace().nth(1) else {
                        return;
                    };
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(target).await else {
                        return;
                    };
                    if client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
    });

    (rx.recv().unwrap(), cert_der)
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
fn test_compio_proxy_selector_is_snapshotted_once_per_dispatch() {
    let target_addr = start_server_tokio();
    let proxy_addr = start_http_proxy_tokio();
    let selector_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = selector_calls.clone();
    let proxy = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap();
    let settings = aioduct::ProxySettings::default().custom(move |_uri| {
        observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(proxy.clone())
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_settings(settings)
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/snapshot"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(selector_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
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
fn compio_ipv4_force_addr_is_the_effective_socks4_destination_for_an_ipv6_origin() {
    let target_addr = start_server_tokio();

    for scheme in ["socks4", "socks4a"] {
        let (proxy_addr, captured_target) = start_forced_socks4_proxy_tokio();
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
                .proxy(
                    aioduct::proxy::ProxyConfig::socks4(&format!("{scheme}://{proxy_addr}"))
                        .unwrap(),
                )
                .timeout(Duration::from_secs(2))
                .build_local()
                .unwrap();
            let logical_url = "http://[2001:db8::1]:8080/forced";

            let no_override = client
                .get_local(logical_url)
                .unwrap()
                .send()
                .await
                .unwrap_err();
            assert!(no_override.to_string().contains("IPv6"), "{no_override}");

            let ipv6_override = client
                .get_local(logical_url)
                .unwrap()
                .force_addr("[::1]:8080".parse().unwrap())
                .send()
                .await
                .unwrap_err();
            assert!(
                ipv6_override.to_string().contains("force_addr"),
                "{ipv6_override}"
            );

            let response = client
                .get_local(logical_url)
                .unwrap()
                .force_addr(target_addr)
                .send()
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "hello aioduct");
        });

        assert_eq!(
            captured_target
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            target_addr
        );
    }
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
fn test_compio_custom_proxy_is_resolved_once_per_dispatch_attempt() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let target_addr = start_server_tokio();
    let proxy_addr = start_http_proxy_tokio();
    let resolutions = Arc::new(AtomicUsize::new(0));
    let observed_resolutions = resolutions.clone();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let settings = aioduct::ProxySettings::default().custom(move |_uri| {
            observed_resolutions.fetch_add(1, Ordering::SeqCst);
            Some(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        });
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_settings(settings)
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/custom-proxy"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });

    assert_eq!(resolutions.load(Ordering::SeqCst), 1);
}

#[test]
fn test_compio_http10_connect_response_local() {
    let target_addr = start_server_tokio();
    let proxy_addr = start_http10_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{target_addr}/http10-connect"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn test_compio_two_http_proxy_hops_local() {
    let target_addr = start_server_tokio();
    let second_addr = start_http_proxy_tokio();
    let first_addr = start_http_proxy_tokio();
    let chain = aioduct::proxy::ProxyChain::new(vec![
        aioduct::proxy::ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
        aioduct::proxy::ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
    ]);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_chain(chain)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{target_addr}/two-http-proxies"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_https_proxy_negotiates_http1_local() {
    let target_addr = start_server_tokio();
    let (proxy_addr, proxy_cert) = start_https_proxy_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&proxy_cert),
        );
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(
                aioduct::proxy::ProxyConfig::https(&format!(
                    "https://localhost:{}",
                    proxy_addr.port()
                ))
                .unwrap(),
            )
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/https-proxy"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_https_proxy_without_alpn_defaults_to_http1_local() {
    let target_addr = start_server_tokio();
    let (proxy_addr, proxy_cert) = start_https_proxy_tokio_without_alpn();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&proxy_cert),
        );
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy(
                aioduct::proxy::ProxyConfig::https(&format!(
                    "https://localhost:{}",
                    proxy_addr.port()
                ))
                .unwrap(),
            )
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let response = client
            .get_local(&format!("http://{target_addr}/https-proxy-no-alpn"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_two_https_proxy_hops_local() {
    let target_addr = start_server_tokio();
    let (second_addr, second_cert) = start_https_proxy_tokio();
    let (first_addr, first_cert) = start_https_proxy_tokio();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(first_cert).unwrap();
    roots.add(second_cert).unwrap();
    let mut config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(std::sync::Arc::new(config));
    let chain = aioduct::proxy::ProxyChain::new(vec![
        aioduct::proxy::ProxyConfig::https(&format!("https://localhost:{}", first_addr.port()))
            .unwrap(),
        aioduct::proxy::ProxyConfig::https(&format!("https://localhost:{}", second_addr.port()))
            .unwrap(),
    ]);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .proxy_chain(chain)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let response = client
            .get_local(&format!("http://{target_addr}/two-https-proxies"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    });
}

#[cfg(all(feature = "rustls", feature = "rustls-aws-lc-rs"))]
#[test]
fn test_compio_ech_https_proxy_hops_fail_before_dns_or_connector_io_local() {
    for https_hop in 0..2 {
        let resolver_attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolver_counter = std::sync::Arc::clone(&resolver_attempts);
        let connector = EchPreflightLocalConnector::default();
        let proxies = if https_hop == 0 {
            vec![
                aioduct::ProxyConfig::https("https://first-proxy.test:8443").unwrap(),
                aioduct::ProxyConfig::http("http://second-proxy.test:8080").unwrap(),
            ]
        } else {
            vec![
                aioduct::ProxyConfig::http("http://first-proxy.test:8080").unwrap(),
                aioduct::ProxyConfig::https("https://second-proxy.test:8443").unwrap(),
            ]
        };
        let error = compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = HttpEngineLocal::<CompioRuntime, EchPreflightLocalConnector>::builder_with_connector(
                connector.clone(),
            )
            .tls(ech_grease_connector())
            .proxy_chain(aioduct::ProxyChain::new(proxies))
            .resolver(move |_host: &str, _port: u16| {
                resolver_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok("127.0.0.1:9".parse().unwrap()) })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = std::io::Result<std::net::SocketAddr>,
                                > + Send,
                        >,
                    >
            })
            .build_local()
            .unwrap();

            client
                .get_local("http://origin.test/ech-preflight")
                .unwrap()
                .send()
                .await
                .unwrap_err()
        });

        assert!(
            error
                .to_string()
                .contains("cannot inherit an ECH-enabled origin configuration"),
            "unexpected ECH preflight error for hop {https_hop}: {error}"
        );
        assert_eq!(
            resolver_attempts.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            connector.attempts.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
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
