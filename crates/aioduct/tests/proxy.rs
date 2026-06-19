#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::runtime::{ConnectorSend, SocketConfig};

use aioduct_test_server::h1::{h1_server, h1_server_with};
use aioduct_test_server::raw::raw_server;

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering as AtomicOrdering};

#[tokio::test]
async fn test_http_proxy() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[tokio::test]
async fn test_proxy_settings_no_proxy_bypass() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;

    // Second target also accessible through the proxy.
    let (other_addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("proxied-ok"))))
    })
    .await;

    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new(&format!("{}", target_addr.ip())));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    // Request to bypassed host goes direct.
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    // Request to non-bypassed host goes through proxy.
    let resp = client
        .get(&format!("http://{other_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "proxied-ok");
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

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
async fn test_no_proxy_ip_cidr_and_port_matching() {
    let no_proxy =
        aioduct::NoProxy::new("127.0.0.1:8080, 10.0.0.0/8, 2001:db8::/32, [2001:db9::5]:8443");

    assert!(no_proxy.matches("127.0.0.1:8080"));
    assert!(!no_proxy.matches("127.0.0.1:8081"));
    assert!(no_proxy.matches("10.20.30.40"));
    assert!(!no_proxy.matches("192.0.2.1"));
    assert!(no_proxy.matches("2001:db8::1234"));
    assert!(!no_proxy.matches("2001:db9::1234"));
    assert!(no_proxy.matches("[2001:db9::5]:8443"));
    assert!(!no_proxy.matches("[2001:db9::5]:443"));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
                // socks5:// resolves DNS locally: IPv4 or IPv6
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
                assert_eq!(buf[0], 0x05); // SOCKS5

                client.write_all(&[0x05, 0x00]).await.unwrap();

                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);
                assert_eq!(buf[0], 0x05); // SOCKS5
                assert_eq!(buf[1], 0x01); // CONNECT
                assert_eq!(buf[3], 0x03); // Domain (socks5h:// sends hostname)

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

#[ignore = "needs CONNECT tunnel rewrite: proxy and target must be separate servers"]
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .build()
        .unwrap();

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
    // With CONNECT tunnel, the target directly receives the Host header.
    let (target_addr, _counter) = h1_server_with(|req| async move {
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
    let (proxy_addr, _conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("host="), "expected host in body, got: {body}");
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

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

#[ignore = "needs CONNECT tunnel rewrite: proxy and target must be separate servers"]
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

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
    // When no explicit port is given, CONNECT should include :443 for HTTPS.
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let _ = client
        .get("https://hyper.rs.local/path")
        .unwrap()
        .send()
        .await;

    let target = connect_target.lock().unwrap().clone();
    assert_eq!(
        target, "hyper.rs.local:443",
        "CONNECT should include port 443 for HTTPS when not explicit in the URL"
    );
}

#[test]
fn test_socks5h_constructor() {
    assert!(
        aioduct::ProxyConfig::socks5h("socks5h://proxy.example.com:1080").is_ok(),
        "socks5h:// should be accepted"
    );
}

#[test]
fn test_socks5h_constructor_rejects_wrong_scheme() {
    assert!(aioduct::ProxyConfig::socks5h("socks5://proxy.example.com:1080").is_err());
    assert!(aioduct::ProxyConfig::socks5h("http://proxy.example.com:1080").is_err());
}

#[test]
fn test_https_proxy_constructor() {
    assert!(
        aioduct::ProxyConfig::https("https://proxy.example.com:443").is_ok(),
        "https:// should be accepted"
    );
}

#[test]
fn test_https_proxy_constructor_rejects_wrong_scheme() {
    assert!(aioduct::ProxyConfig::https("http://proxy.example.com:443").is_err());
    assert!(aioduct::ProxyConfig::https("socks5://proxy.example.com:443").is_err());
}

#[test]
fn test_socks5_constructor_without_port() {
    // Should accept URI without explicit port (defaults to 1080)
    assert!(
        aioduct::ProxyConfig::socks5("socks5://proxy.example.com").is_ok(),
        "socks5:// without port should be accepted"
    );
}

#[test]
fn test_https_proxy_constructor_without_port() {
    // Should accept URI without explicit port (defaults to 443)
    assert!(
        aioduct::ProxyConfig::https("https://proxy.example.com").is_ok(),
        "https:// without port should be accepted"
    );
}

// --- Integration tests ---

/// Serializes env var mutations in integration tests.
static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn system_proxy_integration() {
    let connect_seen = Arc::new(AtomicBool::new(false));
    let connect_seen_clone = connect_seen.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let connect_seen = connect_seen_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);
            if req_str.starts_with("CONNECT") {
                connect_seen.store(true, AtomicOrdering::SeqCst);
            }
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let proxy_url = format!("http://{proxy_addr}");

    {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::set_var("HTTP_PROXY", &proxy_url);
            std::env::set_var("HTTPS_PROXY", &proxy_url);
            std::env::remove_var("NO_PROXY");
            std::env::remove_var("no_proxy");
        }
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .system_proxy()
        .build()
        .unwrap();

    let result = client
        .get("https://hyper.rs.local/prox")
        .unwrap()
        .send()
        .await;

    {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("http_proxy");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("https_proxy");
        }
    }

    // HTTPS request should trigger a CONNECT tunnel through the proxy
    assert!(
        connect_seen.load(AtomicOrdering::SeqCst),
        "system_proxy should route HTTPS request through proxy CONNECT"
    );
    // The request itself should fail because our raw server returns 400
    assert!(result.is_err(), "expected tunnel to fail with 400");
}

#[ignore = "needs CONNECT tunnel rewrite: proxy and target must be separate servers"]
#[tokio::test]
async fn proxy_chain_integration() {
    // Target server that returns a distinctive response
    let (target_addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target-reached"))))
    })
    .await;

    // Proxy server that echoes back the request URI (acting as first chain hop)
    let (proxy_addr, _) = h1_server_with(|req| async move {
        let uri = req.uri().to_string();
        let body = format!("via-chain: {uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let chain = aioduct::ProxyChain::single(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    );

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(chain)
        .build()
        .unwrap();

    // Request is sent through the chain (proxy hop 1) to the target (hop 2)
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("via-chain:"),
        "expected chain proxy response, got: {body}"
    );
}

#[tokio::test]
async fn no_proxy_cidr_integration() {
    // Target server on localhost
    let (target_addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    // A "proxy" server that labels responses
    let (proxy_addr, _) = h1_server_with(|req| async move {
        let uri = req.uri().to_string();
        let body = format!("proxied: {uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    // NoProxy with CIDR 127.0.0.0/8 — covers all localhost IPs
    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new("127.0.0.0/8"));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    // Request to localhost target should bypass the proxy
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "direct");
}

#[ignore = "needs CONNECT tunnel rewrite: proxy and target must be separate servers"]
#[tokio::test]
async fn no_proxy_port_specific() {
    // Verify port-specific NoProxy matching: host:1234 only bypasses for port 1234
    let (target_addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    // Proxy server
    let (proxy_addr, _) = h1_server_with(|req| async move {
        let uri = req.uri().to_string();
        let body = format!("proxied: {uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    // NoProxy matching the target IP but only on a DIFFERENT port (target_port + 1)
    let target_ip = target_addr.ip().to_string();
    let non_matching_port = target_addr.port() + 1;
    let no_proxy_rule = format!("{target_ip}:{non_matching_port}");

    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new(&no_proxy_rule));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    // Request to the target's actual port should NOT be bypassed (port mismatch)
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("proxied:"),
        "expected proxied (port mismatch), got: {body}"
    );
}

#[tokio::test]
async fn proxy_failure_dns() {
    // Use a hostname that will never resolve to trigger DNS failure
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http("http://this.hostname.does.not.exist.invalid:80").unwrap(),
        )
        .build()
        .unwrap();

    let err = client
        .get("http://example.com/path")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        err.is_dns(),
        "expected DNS error, got: {err} (is_dns={})",
        err.is_dns()
    );
}

#[tokio::test]
async fn proxy_failure_connection_refused() {
    // Bind a port, get its address, then drop the listener so the port is closed
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{addr}")).unwrap())
        .build()
        .unwrap();

    let err = client
        .get("http://example.com/path")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        err.is_connect(),
        "expected connect error, got: {err} (is_connect={})",
        err.is_connect()
    );
}

// ── Proxy edge-case tests ─────────────────────────────────────────────

#[tokio::test]
async fn proxy_settings_custom_with_no_proxy_precedence() {
    // Verify no_proxy is checked before custom (settings.rs:96).
    let (proxy_addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("proxied"))))
    })
    .await;

    let (target_addr, _counter) = h1_server().await;

    let called = Arc::new(AtomicBool::new(false));
    let called2 = called.clone();

    let settings = aioduct::ProxySettings::default()
        .custom(move |_url| {
            called2.store(true, AtomicOrdering::SeqCst);
            Some(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        })
        .no_proxy(aioduct::NoProxy::new("127.0.0.1"));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    // Target is localhost — no_proxy matches, custom should NOT be called.
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
    assert!(!called.load(AtomicOrdering::SeqCst));
}

#[tokio::test]
async fn proxy_with_redirect_routing() {
    // Target server (behind proxy).
    let (target_addr, _counter) = h1_server().await;

    // Server that redirects to the target.
    let (redirect_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{target_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    // Raw TCP proxy that counts connections and forwards to the redirect server.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let proxy_req_count = Arc::new(AtomicUsize::new(0));
    let prc = Arc::clone(&proxy_req_count);

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            prc.fetch_add(1, AtomicOrdering::SeqCst);
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            // Reply with a redirect to the target — the redirect follow-up
            // will also go through the proxy.
            let target = format!("http://{target_addr}/");
            let resp =
                format!("HTTP/1.1 302 Found\r\nlocation: {target}\r\nContent-Length: 0\r\n\r\n");
            stream.write_all(resp.as_bytes()).await.ok();
        }
    });

    // Client configured with the proxy.
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    // Send request to the redirect server through proxy.
    // The redirect server redirects to target; the redirect target also
    // goes through the proxy which redirects again to target (infinite loop).
    // The client will exhaust max_redirects. We verify the proxy saw
    // the request.
    let result = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await;

    // The redirect chain will exhaust max_redirects because the proxy always
    // issues 302 back to the target.
    assert!(
        result.is_err(),
        "redirect loop should exhaust max_redirects"
    );

    // Proxy saw at least the initial request.
    let count = proxy_req_count.load(AtomicOrdering::SeqCst);
    assert!(
        count >= 1,
        "proxy should see the initial request, got {count}"
    );
}

#[tokio::test]
async fn credential_resolver_global_env() {
    // EnvCredentialResolver applies global credentials (ignores key).
    use aioduct::{CredentialResolver, EnvCredentialResolver};

    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_MUTEX.lock().unwrap();

    // The resolver reads from env; using defaults means no proxy-user is set.
    // The resolver is a no-op when no env vars are set.
    // Clear env vars that might have been inherited from the test process.
    // ENV_MUTEX serializes proxy env tests; remove_var is unsafe in Rust 2024.
    unsafe {
        std::env::remove_var("AIODUCT_PROXY_USER");
        std::env::remove_var("AIODUCT_PROXY_PASS");
    }

    let resolver = EnvCredentialResolver;
    let result = resolver.resolve("any-key");
    // With no env vars set, resolver returns None (no credentials).
    assert!(result.is_none());
}

#[ignore = "needs CONNECT tunnel rewrite: proxy and target must be separate servers"]
#[tokio::test]
async fn proxy_connection_pooling_with_counter() {
    // Raw TCP proxy server: counts connections, returns 200 for HTTP.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let conn_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&conn_count);

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            loop {
                let mut buf = vec![0u8; 4096];
                let n = match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nproxied")
                        .await
                        .unwrap();
                    stream.flush().await.unwrap();
                    // Keep-alive: continue reading next request.
                }
            }
        }
    });

    // Target server behind proxy.
    let (target_addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .build()
        .unwrap();

    // Two sequential requests — should reuse proxy connection.
    for _ in 0..2 {
        let resp = client
            .get(&format!("http://{target_addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.bytes().await.unwrap();
    }

    // Pooling may or may not reuse depending on timing — verify at least one
    // connection was established (not zero).
    let count = conn_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        count >= 1,
        "expected at least 1 proxy connection, got {count}"
    );
}

#[tokio::test]
async fn proxy_failure_reset_deterministic() {
    // Raw TCP server that accepts then immediately closes (RST).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // Immediately drop — sends RST, no HTTP response.
        drop(stream);
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{addr}")).unwrap())
        .build()
        .unwrap();

    let result = client.get("http://example.com/path").unwrap().send().await;

    assert!(result.is_err(), "proxy reset should produce an error");
}

// ── CONNECT tunnel proxy tests ────────────────────────────────────────────

#[derive(Clone)]
struct ProxyKeepaliveCountingConnector {
    inner: TcpConnector,
    keepalive_calls: Arc<AtomicU32>,
}

impl ProxyKeepaliveCountingConnector {
    fn new() -> Self {
        Self {
            inner: TcpConnector,
            keepalive_calls: Arc::new(AtomicU32::new(0)),
        }
    }

    fn keepalive_calls(&self) -> u32 {
        self.keepalive_calls.load(AtomicOrdering::SeqCst)
    }
}

struct ProxyKeepaliveCountingStream {
    inner: <TcpConnector as ConnectorSend>::Stream,
    counter: Arc<AtomicU32>,
}

impl SocketConfig for ProxyKeepaliveCountingStream {
    fn set_keepalive(
        &self,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        self.counter.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner.set_keepalive(time, interval, retries)
    }

    fn set_fast_open(&self) -> io::Result<()> {
        self.inner.set_fast_open()
    }
}

impl hyper::rt::Read for ProxyKeepaliveCountingStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for ProxyKeepaliveCountingStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl Unpin for ProxyKeepaliveCountingStream {}

impl ConnectorSend for ProxyKeepaliveCountingConnector {
    type Stream = ProxyKeepaliveCountingStream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let counter = Arc::clone(&self.keepalive_calls);
        async move {
            let stream = inner.connect(addr).await?;
            Ok(ProxyKeepaliveCountingStream {
                inner: stream,
                counter,
            })
        }
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let counter = Arc::clone(&self.keepalive_calls);
        async move {
            let stream = inner.connect_bound(addr, local).await?;
            Ok(ProxyKeepaliveCountingStream {
                inner: stream,
                counter,
            })
        }
    }
}

/// A real CONNECT proxy: accepts CONNECT, responds 200, then relays bytes
/// bidirectionally between client and target. Parses the CONNECT target
/// (`host:port`) to connect to the correct server. Each accepted TCP connection
/// counts as one proxy connection.
async fn connect_proxy() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conn_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&conn_count);

    tokio::spawn(async move {
        loop {
            let (mut client, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            cc.fetch_add(1, AtomicOrdering::SeqCst);

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
                // Parse CONNECT host:port
                let target = head.split_whitespace().nth(1).unwrap_or("");
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                let mut target = TcpStream::connect(target).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut target).await;
            });
        }
    });

    (addr, conn_count)
}

/// HTTP proxy for HTTP target now uses CONNECT tunnel.
/// Proxy and target are separate servers — the proxy relays bytes.
#[tokio::test]
async fn http_proxy_uses_connect_tunnel() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert!(conns.load(AtomicOrdering::SeqCst) >= 1);
}

#[tokio::test]
async fn http_proxy_tunnel_applies_tcp_keepalive_to_proxy_connection() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;

    let connector = ProxyKeepaliveCountingConnector::new();
    let connector_ref = connector.clone();

    let client =
        HttpEngineSend::<TokioRuntime, ProxyKeepaliveCountingConnector>::builder_with_connector(
            connector,
        )
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    assert_eq!(
        connector_ref.keepalive_calls(),
        1,
        "configured tcp_keepalive should apply to the proxy tunnel TCP stream"
    );
}

/// Two sequential requests through the same CONNECT tunnel both succeed.
#[tokio::test]
async fn connect_tunnel_pooled_reuse() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .build()
        .unwrap();

    for _ in 0..2 {
        let resp = client
            .get(&format!("http://{target_addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    }
    assert!(conns.load(AtomicOrdering::SeqCst) >= 1);
}

/// A TLS CONNECT proxy: the client establishes a TLS connection TO THE PROXY
/// (https:// proxy scheme) before any CONNECT request. Mirrors `connect_proxy`
/// but wraps each accepted connection in a rustls TLS handshake first, so it
/// exercises aioduct's TLS-to-proxy code path. Returns the proxy address and
/// the self-signed cert (for `localhost`) the client must trust.
#[cfg(feature = "rustls")]
async fn tls_connect_proxy() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicUsize>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    aioduct_test_server::tls::install_crypto_provider();

    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();

    let server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
            .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("localhost:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conn_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&conn_count);

    tokio::spawn(async move {
        loop {
            let (tcp, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            cc.fetch_add(1, AtomicOrdering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                // Client → proxy TLS handshake must complete first.
                let mut client = match acceptor.accept(tcp).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let mut buf = vec![0u8; 8192];
                let n = match client.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let head = String::from_utf8_lossy(&buf[..n]);
                if !head.starts_with("CONNECT ") {
                    return;
                }
                let target = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                let mut upstream = match TcpStream::connect(&target).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    (addr, cert_der, conn_count)
}

/// End-to-end TLS-to-proxy: a plaintext HTTP target reached through an
/// `https://` proxy. This forces the client to perform a real TLS handshake
/// to the proxy (SNI = proxy host) and then CONNECT-tunnel over that encrypted
/// channel. Verifies `ProxyConfig::https()` actually works against a live TLS
/// proxy, not just that it parses.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_tls_to_proxy_reaches_http_target() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, proxy_cert, conns) = tls_connect_proxy().await;

    // Client must trust the proxy's self-signed cert. The proxy URL uses
    // `localhost` (the cert's SAN), so SNI verification to the proxy passes.
    let client_config = aioduct_test_server::tls::make_client_config(&proxy_cert);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert!(
        conns.load(AtomicOrdering::SeqCst) >= 1,
        "the TLS proxy should have accepted at least one connection"
    );
}

/// Build a client TLS config that trusts multiple self-signed certs. The
/// double-TLS path (https target through https proxy) uses one connector for
/// both the proxy handshake and the origin handshake, so its root store must
/// trust both certs.
#[cfg(feature = "rustls")]
fn client_config_trusting(
    certs: &[rustls::pki_types::CertificateDer<'static>],
) -> std::sync::Arc<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    for cert in certs {
        root_store.add(cert.clone()).unwrap();
    }
    let mut config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    std::sync::Arc::new(config)
}

/// End-to-end double-TLS: an HTTPS target reached through an `https://` proxy.
/// This is the other branch of the HTTPS-proxy connect logic
/// (`connect_tunnel_send`): client→proxy TLS, then HTTP CONNECT over that pipe,
/// then a second client→origin TLS handshake *inside* the tunnel. The proxy
/// relays raw bytes, so the inner origin TLS is opaque to it. The single client
/// connector must trust both the proxy cert and the origin cert.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_tls_to_proxy_reaches_https_target() {
    let (origin_addr, origin_cert, _origin_counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let (proxy_addr, proxy_cert, conns) = tls_connect_proxy().await;

    // One connector trusting both the proxy and the origin certs.
    let client_config = client_config_trusting(&[proxy_cert, origin_cert]);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    // The origin uses a `localhost` cert, so the CONNECT target and origin SNI
    // must be `localhost` for verification to pass inside the tunnel.
    let resp = client
        .get(&format!("https://localhost:{}/", origin_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello tls");
    assert!(
        conns.load(AtomicOrdering::SeqCst) >= 1,
        "the TLS proxy should have accepted at least one connection"
    );
}
