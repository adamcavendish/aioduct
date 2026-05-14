#![cfg(feature = "tokio")]
//! Tests verifying HTTP protocol-specific behavior differences between HTTP/1.1
//! and HTTP/2, as well as TLS-related semantics.

use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

fn h1_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
}

fn h2_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(5))
        .build()
}

// ═══════════════════════════════════════════════════════════════════════════════
// HTTP/1.1 Specifics
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify the client correctly decodes a chunked Transfer-Encoding response.
#[tokio::test]
async fn h1_chunked_transfer_encoding() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
          5\r\nhello\r\n\
          6\r\n world\r\n\
          0\r\n\r\n"
            .to_vec()
    })
    .await;

    let client = h1_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "hello world",
        "chunked body should be reassembled correctly"
    );
}

/// Verify the client reads exactly Content-Length bytes from a raw response.
#[tokio::test]
async fn h1_content_length_body() {
    let payload = "exact 42 bytes of payload for this test!!";
    assert_eq!(payload.len(), 41); // sanity

    let addr = aioduct_test_server::raw::raw_server(move |_req| async move {
        let body = "exact 42 bytes of payload for this test!!";
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes()
    })
    .await;

    let client = h1_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body.len(), 41);
    assert_eq!(body, "exact 42 bytes of payload for this test!!");
}

/// HEAD requests must return an empty body even when Content-Length is set.
#[tokio::test]
async fn h1_head_request_no_body() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|req| async move {
        if req.method() == http::Method::HEAD {
            let resp = http::Response::builder()
                .header("Content-Length", "1000")
                .body(http_body_util::Full::new(bytes::Bytes::new()))
                .unwrap();
            Ok::<_, std::convert::Infallible>(resp)
        } else {
            let resp = http::Response::builder()
                .body(http_body_util::Full::new(bytes::Bytes::from(
                    "should not see this",
                )))
                .unwrap();
            Ok(resp)
        }
    })
    .await;

    let client = h1_client();
    let url = format!("http://{addr}/");

    let resp = client
        .request(http::Method::HEAD, &url)
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    // Content-Length header should be present
    assert_eq!(
        resp.headers()
            .get("content-length")
            .map(|v| v.to_str().unwrap()),
        Some("1000"),
        "HEAD response should include Content-Length header"
    );
    // Body must be empty for HEAD
    let body = resp.bytes().await.unwrap();
    assert!(
        body.is_empty(),
        "HEAD response body must be empty, got {} bytes",
        body.len()
    );
}

/// When server sends `Connection: close`, the client should not reuse the connection.
#[tokio::test]
async fn h1_keep_alive_header_respected() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let conn_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let conn_count2 = conn_count.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            conn_count2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    // Check if we got a full request
                    if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        let resp =
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                        let _ = stream.write_all(resp).await;
                        let _ = stream.flush().await;
                        // Close from server side after writing response
                        let _ = stream.shutdown().await;
                        return;
                    }
                }
            });
        }
    });

    let client = h1_client();
    let url = format!("http://{addr}/");

    // First request
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Brief pause to let connection return to pool / get evicted
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second request — should open a NEW connection because server said close
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let conns = conn_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        conns, 2,
        "Connection: close should prevent reuse; expected 2 connections, got {conns}"
    );
}

/// Multiple sequential H1 requests with Connection: keep-alive should reuse the connection.
#[tokio::test]
async fn h1_keep_alive_reuses_connection() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = h1_client();
    let url = format!("http://{addr}/");

    for _ in 0..5 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "keep-alive should reuse single connection for 5 sequential requests"
    );
}

/// Verify content-length mismatch (server sends fewer bytes) is handled.
#[tokio::test]
async fn h1_content_length_mismatch_short() {
    // Server claims Content-Length: 100 but only sends 5 bytes then closes.
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nhello".to_vec()
    })
    .await;

    let client = h1_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    // Reading the body should fail because the connection closes prematurely
    let result = resp.bytes().await;
    assert!(
        result.is_err(),
        "reading body with content-length mismatch should error"
    );
}

/// Verify response with no Content-Length and no Transfer-Encoding (HTTP/1.0 style
/// read-until-close) is handled.
#[tokio::test]
async fn h1_read_until_close() {
    let addr = aioduct_test_server::raw::raw_server(|_req| async {
        // No Content-Length, no Transfer-Encoding — body ends when connection closes
        b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nread until EOF".to_vec()
    })
    .await;

    let client = h1_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "read until EOF");
}

// ═══════════════════════════════════════════════════════════════════════════════
// HTTP/2 Specifics
// ═══════════════════════════════════════════════════════════════════════════════

/// Verify h2c (HTTP/2 cleartext) works with `.http2_prior_knowledge()`.
#[tokio::test]
async fn h2_prior_knowledge_cleartext() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "response should use HTTP/2"
    );
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
    assert_eq!(counter.connections(), 1);
    assert_eq!(counter.requests(), 1);
}

/// GOAWAY while a request is in-flight should allow that request to complete gracefully.
#[tokio::test]
async fn h2_goaway_graceful_in_flight() {
    // h2_goaway_after(1) sends GOAWAY after processing 1 request
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_after(1).await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    // First request completes normally (then server sends GOAWAY)
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");

    // Wait for GOAWAY to be received
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second request should still succeed (client opens new connection after GOAWAY)
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");

    // Should have used 2 connections (first one was shut down with GOAWAY)
    assert!(
        counter.connections() >= 2,
        "expected >= 2 connections after GOAWAY, got {}",
        counter.connections()
    );
}

/// 10 sequential requests over H2 should all succeed and reuse the same connection.
#[tokio::test]
async fn h2_stream_count_sequential() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    for i in 0..10 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200, "request {i} should succeed");
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(
        counter.connections(),
        1,
        "10 sequential H2 requests should multiplex on 1 connection"
    );
    assert_eq!(counter.requests(), 10);
}

/// Concurrent H2 requests should all succeed via stream multiplexing.
#[tokio::test]
async fn h2_stream_count_concurrent() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    // Warm the connection first
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Fire 10 concurrent requests
    let mut handles = Vec::new();
    for _ in 0..10 {
        let c = client.clone();
        let u = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = c.get(&u).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // All 11 requests (1 warmup + 10 concurrent) should have completed
    assert_eq!(counter.requests(), 11);
}

/// H2 server with immediate GOAWAY after first connection (simulating graceful restart)
/// should not lose the client's response.
#[tokio::test]
async fn h2_goaway_immediate_still_responds() {
    let (addr, counter) = aioduct_test_server::h2::h2_goaway_immediate().await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    // The server serves requests, then sends GOAWAY after connection completes.
    // The first request should still get a valid response.
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");
    assert_eq!(counter.requests(), 1);
}

/// Verify that h2_prior_knowledge fails gracefully against an HTTP/1 server.
#[tokio::test]
async fn h2_prior_knowledge_against_h1_server_fails() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    // H2 client sending preface to an H1 server should fail
    let result = client.get(&url).unwrap().send().await;
    assert!(
        result.is_err(),
        "h2 prior knowledge against h1 server should fail"
    );
}

/// HTTP/2 responses should report version as HTTP/2.
#[tokio::test]
async fn h2_response_version_is_h2() {
    let (addr, _counter) = aioduct_test_server::h2::h2_server().await;
    let client = h2_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "H2 response should report HTTP/2 version"
    );
    let _ = resp.text().await.unwrap();
}

/// HTTP/1.1 responses should report version as HTTP/1.1.
#[tokio::test]
async fn h1_response_version_is_h11() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let client = h1_client();
    let url = format!("http://{addr}/");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(
        resp.version(),
        http::Version::HTTP_11,
        "H1 response should report HTTP/1.1 version"
    );
    let _ = resp.text().await.unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════════
// TLS Behavior (cfg-gated on feature = "rustls")
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "rustls")]
mod tls_tests {
    use super::*;

    fn install_provider() {
        aioduct_test_server::tls::install_crypto_provider();
    }

    /// HTTPS with h2 ALPN should negotiate HTTP/2.
    #[tokio::test]
    async fn tls_h2_alpn_negotiation() {
        install_provider();

        let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;
        let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
        let connector = aioduct::tls::RustlsConnector::new(client_config);

        let client: HttpEngineSend<TokioRuntime, TcpConnector> =
            HttpEngineSend::builder(TcpConnector)
                .tls(connector)
                .timeout(Duration::from_secs(5))
                .build();

        let resp = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.version(),
            http::Version::HTTP_2,
            "TLS with h2 ALPN should negotiate HTTP/2"
        );
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello tls");
    }

    /// Server offering only http/1.1 ALPN should result in HTTP/1.1 connection.
    #[tokio::test]
    async fn tls_h1_fallback() {
        install_provider();

        // Server only offers http/1.1
        let (addr, cert_der, _counter) =
            aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;

        // Client config offers both h2 and http/1.1
        let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
        let connector = aioduct::tls::RustlsConnector::new(client_config);

        let client: HttpEngineSend<TokioRuntime, TcpConnector> =
            HttpEngineSend::builder(TcpConnector)
                .tls(connector)
                .timeout(Duration::from_secs(5))
                .build();

        let resp = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.version(),
            http::Version::HTTP_11,
            "server offering only http/1.1 ALPN should fall back to HTTP/1.1"
        );
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello tls");
    }

    /// Server with no ALPN at all should still connect (graceful fallback).
    #[tokio::test]
    async fn tls_no_alpn_fallback() {
        install_provider();

        // Server with empty ALPN list — no protocol negotiation
        let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[]).await;

        let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
        let connector = aioduct::tls::RustlsConnector::new(client_config);

        let client: HttpEngineSend<TokioRuntime, TcpConnector> =
            HttpEngineSend::builder(TcpConnector)
                .tls(connector)
                .timeout(Duration::from_secs(5))
                .build();

        let result = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await;

        // Should either succeed with HTTP/1.1 fallback, or fail gracefully
        // (not panic or hang). Some implementations may error on no ALPN match.
        match result {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                // Without ALPN, should fall back to HTTP/1.1
                assert_eq!(
                    resp.version(),
                    http::Version::HTTP_11,
                    "no ALPN should fall back to HTTP/1.1"
                );
                let _ = resp.text().await.unwrap();
            }
            Err(e) => {
                // Acceptable: some TLS implementations reject no-ALPN-match
                let msg = format!("{e}");
                assert!(
                    !msg.contains("timeout"),
                    "no-ALPN should not cause a timeout hang, got: {e}"
                );
            }
        }
    }

    /// A self-signed certificate not in the trust store should be rejected.
    #[tokio::test]
    async fn tls_invalid_cert_rejected() {
        install_provider();

        // Start a TLS server with a self-signed cert
        let (addr, _cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;

        // Use WebPKI roots only — does NOT trust the self-signed cert
        let connector = aioduct::tls::RustlsConnector::with_webpki_roots();

        let client: HttpEngineSend<TokioRuntime, TcpConnector> =
            HttpEngineSend::builder(TcpConnector)
                .tls(connector)
                .timeout(Duration::from_secs(5))
                .build();

        let result = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await;

        assert!(
            result.is_err(),
            "self-signed cert without trust root should be rejected, but got: {:?}",
            result.as_ref().map(|r| r.status())
        );
    }

    /// TLS connection should include TLS info on the response.
    #[tokio::test]
    async fn tls_response_has_tls_info() {
        install_provider();

        let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;
        let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
        let connector = aioduct::tls::RustlsConnector::new(client_config);

        let client: HttpEngineSend<TokioRuntime, TcpConnector> =
            HttpEngineSend::builder(TcpConnector)
                .tls(connector)
                .timeout(Duration::from_secs(5))
                .build();

        let resp = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        // TLS info should be available on the response
        assert!(
            resp.tls_info().is_some(),
            "TLS response should include TLS info"
        );
        let _ = resp.text().await.unwrap();
    }

    /// Multiple TLS requests to the same host should reuse the connection.
    #[tokio::test]
    async fn tls_connection_reuse() {
        install_provider();

        let (addr, cert_der, counter) = aioduct_test_server::tls::tls_h2_server().await;
        let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
        let connector = aioduct::tls::RustlsConnector::new(client_config);

        let client: HttpEngineSend<TokioRuntime, TcpConnector> =
            HttpEngineSend::builder(TcpConnector)
                .tls(connector)
                .pool_idle_timeout(Duration::from_secs(60))
                .timeout(Duration::from_secs(5))
                .build();

        let url = format!("https://localhost:{}/", addr.port());

        for _ in 0..3 {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }

        assert_eq!(
            counter.connections(),
            1,
            "TLS H2 requests should reuse a single connection"
        );
    }
}
