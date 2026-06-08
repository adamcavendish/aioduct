#![cfg(all(test, feature = "tokio"))]
#[cfg(all(test, feature = "tokio"))]
mod tokio_tests {
    use crate::client::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};

    /// Helper: build an HttpEngineSend with default settings (no h2c).
    fn make_engine() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap()
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_success_200() {
        // Simulate a CONNECT proxy that responds with 200 OK then drops
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the CONNECT request
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Verify it's a CONNECT request
            let req_str = String::from_utf8_lossy(&request);
            assert!(
                req_str.starts_with("CONNECT "),
                "should be a CONNECT request"
            );
            assert!(
                req_str.contains("target.example.com:443"),
                "should target the correct host"
            );

            // Respond with 200
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();

            // Drop the connection immediately so TLS handshake fails with EOF
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        // connect_tunnel will succeed the CONNECT handshake but then try TLS
        // which will fail since no TLS connector is configured (make_engine() has no TLS)
        let result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;
        assert!(
            result.is_err(),
            "should fail because no TLS connector configured"
        );
    }

    #[cfg(not(feature = "rustls"))]
    #[tokio::test]
    async fn connect_tunnel_requires_rustls_feature() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let req_str = String::from_utf8_lossy(&request);
            assert!(req_str.starts_with("CONNECT target.example.com:443 "));
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        let result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;
        match result {
            Err(crate::Error::Tls(err)) => {
                assert!(
                    err.to_string()
                        .contains("requires the `rustls` TLS backend feature")
                );
            }
            Ok(_) => panic!("CONNECT tunnel unexpectedly succeeded without rustls"),
            Err(err) => panic!("expected TLS feature error, got {err}"),
        }
        proxy_task.await.unwrap();
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_defaults_port_443_when_authority_has_no_port() {
        // When the URL has no explicit port (e.g. https://example.com/),
        // the authority is "example.com" without ":443".
        // connect_tunnel must add the port so CONNECT targets the right port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        // Authority WITHOUT explicit port — connect_tunnel must add :443
        let target_authority: http::uri::Authority = "target.example.com".parse().unwrap();

        // TLS will fail (no TLS connector in make_engine), but the CONNECT
        // handshake should succeed and the capture should show the target
        // includes :443.
        let _result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;

        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("CONNECT target.example.com:443"),
            "CONNECT target must include port 443 when authority lacks explicit port, got: {captured}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_defaults_port_443_for_ipv6_without_port() {
        // IPv6 authorities like "[::1]" must still get ":443" appended.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "[::1]".parse().unwrap();

        let _result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;

        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("CONNECT [::1]:443"),
            "IPv6 CONNECT target must include port 443, got: {captured}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_preserves_explicit_port() {
        // When the authority already has an explicit port, it must be kept.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "example.com:8443".parse().unwrap();

        let _result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;

        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("CONNECT example.com:8443"),
            "CONNECT target must preserve explicit port 8443, got: {captured}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_proxy_returns_403() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Respond with 403 Forbidden
            server_io
                .write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n")
                .await
                .unwrap();
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        let result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("CONNECT tunnel failed"),
            "error should mention tunnel failure, got: {err}"
        );
        assert!(
            err.contains("403"),
            "error should contain the status code, got: {err}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_proxy_closes_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the full CONNECT request first
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Drop without sending any response - client sees EOF during read
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        let result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("proxy closed connection"),
            "error should mention proxy closure, got: {err}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_sends_proxy_auth_header() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the full CONNECT request
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            // Respond with 200
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            // Drop to trigger TLS failure
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080")
            .unwrap()
            .basic_auth("user", "password");
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        // connect_tunnel will succeed the CONNECT handshake, send auth header,
        // then TLS fails because no TLS connector configured
        let _result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;

        // Verify the captured request contains the Proxy-Authorization header
        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("Proxy-Authorization: Basic"),
            "CONNECT request should include Proxy-Authorization header, got: {captured}"
        );
        assert!(
            captured.contains("CONNECT target.example.com:443"),
            "CONNECT request should target the correct host, got: {captured}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_response_too_large() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the full CONNECT request
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Send a huge response (> 8192 bytes) without ending with

            let big_chunk = vec![b'A'; 9000];
            server_io.write_all(&big_chunk).await.unwrap();
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        let result = engine
            .connect_tunnel_send(tcp_stream, &proxy, &target_authority, None)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("too large"),
            "error should mention response too large, got: {err}"
        );
    }

    // --- connect_via_proxy_chain tests ---

    #[tokio::test]
    async fn connect_via_proxy_chain_empty_is_error() {
        let engine = make_engine();
        let chain = crate::proxy::ProxyChain::new(vec![]);
        let authority: http::uri::Authority = "example.com:443".parse().unwrap();
        let result = engine
            .connect_via_proxy_chain_send(&chain, &authority, true, None, false)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("empty"),
            "expected empty chain error, got: {err}"
        );
    }

    #[tokio::test]
    async fn connect_via_proxy_chain_three_hops_is_error() {
        let engine = make_engine();
        let p1 = crate::proxy::ProxyConfig::http("http://p1:8080").unwrap();
        let p2 = crate::proxy::ProxyConfig::socks5("socks5://p2:1080").unwrap();
        let p3 = crate::proxy::ProxyConfig::http("http://p3:3128").unwrap();
        let chain = crate::proxy::ProxyChain::new(vec![p1, p2, p3]);
        let authority: http::uri::Authority = "example.com:443".parse().unwrap();
        let result = engine
            .connect_via_proxy_chain_send(&chain, &authority, true, None, false)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("longer than 2 hops"),
            "expected chain length error, got: {err}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_two_hop_send_http_http_chain() {
        // Two-hop HTTP CONNECT chain:
        //   client → proxy1 (CONNECT to proxy2) → proxy2 (CONNECT to target)
        //
        // proxy2 returns 200 then stays open (no real TLS server behind it).
        // The client will attempt TLS to the target after both CONNECT
        // handshakes, and that TLS will fail. The key assertion is that the
        // error is NOT "CONNECT tunnel failed" — both tunnels opened.

        // proxy2: responds 200 to CONNECT the-target, then waits
        let proxy2_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy2_addr = proxy2_listener.local_addr().unwrap();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = proxy2_listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains("CONNECT example.com:443"),
                "proxy2 should see CONNECT to target, got: {req}"
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        });

        // proxy1: reads CONNECT to proxy2, connects to proxy2, responds 200,
        // then relays all traffic bidirectionally
        let proxy1_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy1_addr = proxy1_listener.local_addr().unwrap();
        let proxy2_relay = proxy2_addr;

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut client, _) = proxy1_listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = client.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.starts_with("CONNECT"),
                "proxy1 should see CONNECT, got: {req}"
            );

            // Connect to proxy2 to establish the tunnel
            let mut upstream = match tokio::net::TcpStream::connect(proxy2_relay).await {
                Ok(s) => s,
                Err(_) => {
                    let _ = client
                        .write_all(
                            b"HTTP/1.1 502 Bad Gateway

",
                        )
                        .await;
                    return;
                }
            };
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();

            // Relay client ↔ proxy2
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });

        let engine = make_engine();
        let proxy1 = crate::proxy::ProxyConfig::http(&format!("http://{proxy1_addr}")).unwrap();
        let proxy2 = crate::proxy::ProxyConfig::http(&format!("http://{proxy2_addr}")).unwrap();
        let chain = crate::proxy::ProxyChain::new(vec![proxy1, proxy2]);
        let authority: http::uri::Authority = "example.com:443".parse().unwrap();

        let result = engine
            .connect_via_proxy_chain_send(&chain, &authority, true, None, false)
            .await;
        // Should open both tunnels then fail at TLS to the target
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            !err.contains("CONNECT tunnel failed"),
            "both tunnels should succeed, error should be TLS-related, got: {err}"
        );
    }
}
