#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{h1_server, h1_server_with};
use aioduct_test_server::raw::raw_server;

use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

#[tokio::test]
async fn test_http_proxy() {
    let (proxy_addr, _counter) = h1_server_with(|req| async move {
        let uri = req.uri().to_string();
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = format!("proxied: uri={uri} host={host}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build();

    let resp = client
        .get("http://example.com/path")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("proxied:"),
        "expected proxied response, got: {body}"
    );
    assert!(body.contains("/path"), "expected path in URI, got: {body}");
}
#[tokio::test]
async fn test_proxy_settings_no_proxy_bypass() {
    // Set up a "proxy" server that labels responses
    let (proxy_addr, _counter) = h1_server_with(|req| async move {
        let uri = req.uri().to_string();
        let body = format!("proxied: {uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    // Set up the actual target server
    let (target_addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new(&format!("{}", target_addr.ip())));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy_settings(settings)
        .build();

    // Request to the bypassed host goes direct
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "direct");

    // Request to a non-bypassed host goes through proxy
    let resp = client
        .get("http://example.com/test")
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("proxied:"), "expected proxy, got: {body}");
}
#[tokio::test]
async fn test_no_proxy_wildcard_bypasses_all() {
    let (target_addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    let settings =
        aioduct::ProxySettings::all(aioduct::ProxyConfig::http("http://127.0.0.1:9999").unwrap())
            .no_proxy(aioduct::NoProxy::new("*"));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy_settings(settings)
        .build();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "direct");
}
#[tokio::test]
async fn test_no_proxy_domain_suffix_matching() {
    let no_proxy = aioduct::NoProxy::new(".example.com, localhost");

    // Direct matches
    assert!(!no_proxy.matches("example.com")); // no leading dot, exact doesn't match
    assert!(no_proxy.matches("foo.example.com"));
    assert!(no_proxy.matches("bar.baz.example.com"));
    assert!(no_proxy.matches("localhost"));

    // Non-matches
    assert!(!no_proxy.matches("notexample.com"));
    assert!(!no_proxy.matches("other.com"));
}
#[tokio::test]
async fn test_no_proxy_bare_domain_matches_subdomains() {
    let no_proxy = aioduct::NoProxy::new("example.com");

    assert!(no_proxy.matches("example.com"));
    assert!(no_proxy.matches("foo.example.com"));
    assert!(!no_proxy.matches("notexample.com"));
}
#[tokio::test]
async fn test_socks5_proxy() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;

    // Minimal SOCKS5 proxy server
    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut client, _) = socks_listener.accept().await.unwrap();

            tokio::spawn(async move {
                // Read greeting
                let mut buf = [0u8; 256];
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x05); // SOCKS5

                // Reply: no auth
                client.write_all(&[0x05, 0x00]).await.unwrap();

                // Read connect request
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);
                assert_eq!(buf[0], 0x05); // SOCKS5
                assert_eq!(buf[1], 0x01); // CONNECT
                assert_eq!(buf[3], 0x03); // Domain

                let domain_len = buf[4] as usize;
                let port_offset = 5 + domain_len;
                let port = ((buf[port_offset] as u16) << 8) | (buf[port_offset + 1] as u16);

                // Connect to target
                let target = format!("127.0.0.1:{port}");
                let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();

                // Reply: success, bound to 0.0.0.0:0
                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                    .await
                    .unwrap();

                // Bidirectional relay
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .build();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[tokio::test]
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

                // Require username/password auth
                client.write_all(&[0x05, 0x02]).await.unwrap();

                // Read auth sub-negotiation
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x01); // sub-version
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

                // Read connect request
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(
            aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}"))
                .unwrap()
                .basic_auth("testuser", "testpass"),
        )
        .build();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_http_proxy_basic_auth() {
    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_seen_clone = auth_seen.clone();

    let (proxy_addr, _counter) = h1_server_with(move |req| {
        let auth_seen = auth_seen_clone.clone();
        async move {
            // For plain HTTP proxy, Proxy-Authorization should be in the request headers
            if let Some(auth) = req.headers().get("proxy-authorization") {
                let auth_str = auth.to_str().unwrap_or("");
                // "Aladdin:open sesame" -> base64 "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
                if auth_str == "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==" {
                    auth_seen.store(true, AtomicOrdering::SeqCst);
                }
            }
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .build();

    let resp = client
        .get("http://example.com/prox")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        auth_seen.load(AtomicOrdering::SeqCst),
        "proxy should have received Proxy-Authorization header with basic auth"
    );
}

#[tokio::test]
async fn test_http_proxy_preserves_host_header() {
    let (proxy_addr, _counter) = h1_server_with(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let method = req.method().to_string();
        let body = format!("method={method} host={host}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build();

    let resp = client
        .get("http://hyper.rs.local/path")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "expected GET method, got: {body}"
    );
    assert!(
        body.contains("host=hyper.rs.local"),
        "expected host=hyper.rs.local, got: {body}"
    );
}

#[tokio::test]
async fn test_connect_tunnel_includes_proxy_auth() {
    // Simulate a proxy that receives a CONNECT request.
    // We parse the raw CONNECT to check for Proxy-Authorization.
    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_seen_clone = auth_seen.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let auth_seen = auth_seen_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);

            // Check that this is a CONNECT request
            if req_str.starts_with("CONNECT") {
                // Check for Proxy-Authorization header
                for line in req_str.lines() {
                    if line.to_lowercase().starts_with("proxy-authorization:") {
                        let value = line.split_once(':').map(|x| x.1).unwrap_or("").trim();
                        if value == "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==" {
                            auth_seen.store(true, AtomicOrdering::SeqCst);
                        }
                    }
                }
            }

            // Return 400 to avoid dealing with actual TLS tunneling.
            // This will cause an error on the client side, which is expected.
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .build();

    // HTTPS request triggers CONNECT tunnel
    let result = client
        .get("https://hyper.rs.local/prox")
        .unwrap()
        .send()
        .await;

    // The request should fail because our mock proxy returns 400
    assert!(result.is_err(), "expected tunnel error, got success");

    assert!(
        auth_seen.load(AtomicOrdering::SeqCst),
        "CONNECT request should include Proxy-Authorization header"
    );
}

#[tokio::test]
async fn test_connect_tunnel_detects_auth_required() {
    let proxy_addr = raw_server(|req_bytes| async move {
        let req_str = String::from_utf8_lossy(&req_bytes);

        if req_str.starts_with("CONNECT") {
            // Return 407 Proxy Authentication Required
            b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n".to_vec()
        } else {
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build();

    // HTTPS request triggers CONNECT tunnel, which should fail with 407
    let err = client
        .get("https://hyper.rs.local/prox")
        .unwrap()
        .send()
        .await;

    assert!(err.is_err(), "expected error from 407 proxy response");
    let err_msg = format!("{}", err.unwrap_err());
    assert!(
        err_msg.contains("407") || err_msg.contains("CONNECT tunnel failed"),
        "expected tunnel failure message, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_proxy_settings_routes_http_and_https_separately() {
    // Set up an HTTP proxy server
    let (http_proxy_addr, _counter) = h1_server_with(|req| async move {
        let uri = req.uri().to_string();
        let body = format!("http-proxy: {uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    // Set up the actual target server for direct access
    let (target_addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    // Configure separate HTTP proxy, no HTTPS proxy, bypass the target address
    let settings = aioduct::ProxySettings::default()
        .http(aioduct::ProxyConfig::http(&format!("http://{http_proxy_addr}")).unwrap())
        .no_proxy(aioduct::NoProxy::new(&format!("{}", target_addr.ip())));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy_settings(settings)
        .build();

    // HTTP request to non-bypassed host goes through HTTP proxy
    let resp = client
        .get("http://example.com/test")
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.starts_with("http-proxy:"),
        "expected http-proxy response, got: {body}"
    );

    // Request to bypassed host goes direct
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "direct");
}

#[tokio::test]
async fn test_connect_tunnel_target_authority() {
    // Verify the CONNECT request targets the correct host:port
    let connect_target = Arc::new(std::sync::Mutex::new(String::new()));
    let connect_target_clone = connect_target.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let connect_target = connect_target_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);
            if req_str.starts_with("CONNECT") {
                // Parse "CONNECT host:port HTTP/1.1"
                if let Some(target) = req_str.split_whitespace().nth(1) {
                    *connect_target.lock().unwrap() = target.to_string();
                }
            }
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build();

    let _ = client
        .get("https://hyper.rs.local:8443/path")
        .unwrap()
        .send()
        .await;

    let target = connect_target.lock().unwrap().clone();
    assert_eq!(
        target, "hyper.rs.local:8443",
        "CONNECT should target the original host:port"
    );
}

#[tokio::test]
async fn test_connect_tunnel_default_port() {
    // Verify CONNECT uses the authority as-is when no explicit port is given.
    // Note: aioduct passes the raw URI authority to CONNECT (e.g. "hyper.rs.local"
    // without appending ":443"). This is valid -- the proxy infers port 443 for HTTPS.
    let connect_target = Arc::new(std::sync::Mutex::new(String::new()));
    let connect_target_clone = connect_target.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let connect_target = connect_target_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);
            if req_str.starts_with("CONNECT")
                && let Some(target) = req_str.split_whitespace().nth(1)
            {
                *connect_target.lock().unwrap() = target.to_string();
            }
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build();

    let _ = client
        .get("https://hyper.rs.local/path")
        .unwrap()
        .send()
        .await;

    let target = connect_target.lock().unwrap().clone();
    assert_eq!(
        target, "hyper.rs.local",
        "CONNECT should use the raw authority from the URL"
    );
}
