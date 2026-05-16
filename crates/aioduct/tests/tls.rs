#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use hyper::service::service_fn;
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::TokioExec;
use aioduct_test_server::tls::{crypto_provider, install_crypto_provider};

#[cfg(feature = "rustls")]
#[tokio::test]
async fn test_https_local_tls_server() {
    use std::sync::Arc;

    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    server_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let server_tls_config = Arc::new(server_tls_config);
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(server_tls_config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn({
        let tls_acceptor = tls_acceptor.clone();
        async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);
                    let _ = hyper::server::conn::http2::Builder::new(TokioExec)
                        .serve_connection(
                            io,
                            service_fn(|_req| async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "hello tls",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der).unwrap();
    let mut client_tls_config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_root_certificates(root_store)
        .with_no_client_auth();
    client_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config));

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await;

    match &resp {
        Ok(r) => assert_eq!(r.status(), http::StatusCode::OK),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                !msg.contains("timeout"),
                "HTTPS request timed out — TLS/H2 handshake hang: {e}"
            );
            panic!("HTTPS request failed: {e}");
        }
    }
    let body = resp.unwrap().text().await.unwrap();
    assert_eq!(body, "hello tls");
}
#[cfg(feature = "rustls")]
#[tokio::test]
async fn test_https_h1_local_tls_server() {
    use std::sync::Arc;

    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    // Server only offers h1
    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    server_tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let server_tls_config = Arc::new(server_tls_config);
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(server_tls_config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn({
        let tls_acceptor = tls_acceptor.clone();
        async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            io,
                            service_fn(|_req| async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "hello h1 tls",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der).unwrap();
    let mut client_tls_config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_root_certificates(root_store)
        .with_no_client_auth();
    client_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config));

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await;

    match &resp {
        Ok(r) => assert_eq!(r.status(), http::StatusCode::OK),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                !msg.contains("timeout"),
                "HTTPS h1 request timed out — TLS handshake hang: {e}"
            );
            panic!("HTTPS h1 request failed: {e}");
        }
    }
    let body = resp.unwrap().text().await.unwrap();
    assert_eq!(body, "hello h1 tls");
}
#[cfg(feature = "rustls")]
#[tokio::test]
async fn test_https_no_alpn_server() {
    use std::sync::Arc;

    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    // Server with NO ALPN
    let server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    let server_tls_config = Arc::new(server_tls_config);
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(server_tls_config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn({
        let tls_acceptor = tls_acceptor.clone();
        async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            io,
                            service_fn(|_req| async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "hello no-alpn",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der).unwrap();
    let mut client_tls_config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_root_certificates(root_store)
        .with_no_client_auth();
    client_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config));

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await;

    match &resp {
        Ok(r) => assert_eq!(r.status(), http::StatusCode::OK),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                !msg.contains("timeout"),
                "HTTPS no-alpn request timed out — TLS handshake hang: {e}"
            );
            panic!("HTTPS no-alpn request failed: {e}");
        }
    }
    let body = resp.unwrap().text().await.unwrap();
    assert_eq!(body, "hello no-alpn");
}
#[cfg(feature = "rustls")]
#[tokio::test]
async fn test_https_with_webpki_roots_local() {
    use std::sync::Arc;

    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    server_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let server_tls_config = Arc::new(server_tls_config);
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(server_tls_config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn({
        let tls_acceptor = tls_acceptor.clone();
        async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);
                    let _ = hyper::server::conn::http2::Builder::new(TokioExec)
                        .serve_connection(
                            io,
                            service_fn(|_req| async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "hello webpki",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    // Use danger_accept_invalid_certs to mimic with_webpki_roots behavior
    // (default ALPN) but against self-signed cert
    let connector = aioduct::tls::RustlsConnector::danger_accept_invalid_certs();

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await;

    match &resp {
        Ok(r) => assert_eq!(r.status(), http::StatusCode::OK),
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                !msg.contains("timeout"),
                "HTTPS request with default ALPN timed out: {e}"
            );
            panic!("HTTPS request failed: {e}");
        }
    }
    let body = resp.unwrap().text().await.unwrap();
    assert_eq!(body, "hello webpki");
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// HTTPS connection must NOT be reused for plain HTTP request to the same host.
// Pool key must include the scheme. (curl test_01_18)
#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_connection_not_reused_for_http() {
    aioduct_test_server::tls::install_crypto_provider();

    // Start an HTTPS server
    let (tls_addr, cert_der, https_counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    // Start a plain HTTP server
    let (http_addr, http_counter) = aioduct_test_server::h1::h1_server().await;

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder(TcpConnector)
        .tls(connector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build();

    // First: HTTPS request
    let resp = client
        .get(&format!("https://localhost:{}/", tls_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // Second: plain HTTP request — must NOT reuse the HTTPS connection
    let resp = client
        .get(&format!("http://{http_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    assert_eq!(
        https_counter.connections(),
        1,
        "HTTPS server should see 1 connection"
    );
    assert_eq!(
        http_counter.connections(),
        1,
        "HTTP server should see 1 separate connection"
    );
}

// HSTS must upgrade redirect targets, not just the initial URI.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn hsts_upgrades_redirect_targets() {
    aioduct_test_server::tls::install_crypto_provider();

    let (tls_addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |req| async move {
            let path = req.uri().path().to_string();
            let mut resp = if path == "/redirect" {
                let host = req
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("localhost");
                Response::builder()
                    .status(302)
                    .header("Location", format!("http://{host}/final"))
                    .body(Full::new(Bytes::new()))
                    .unwrap()
            } else {
                Response::new(Full::new(Bytes::from("hsts-ok")))
            };
            resp.headers_mut()
                .insert("strict-transport-security", "max-age=3600".parse().unwrap());
            Ok(resp)
        })
        .await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let hsts_store = aioduct::hsts::HstsStore::new();
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder(TcpConnector)
        .tls(connector)
        .hsts(hsts_store.clone())
        .timeout(Duration::from_secs(5))
        .build();

    // 1. Visit HTTPS — STS header seeds the HSTS store with "localhost"
    let resp = client
        .get(&format!("https://localhost:{}/", tls_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "hsts-ok");
    assert!(
        hsts_store.should_upgrade("localhost"),
        "HSTS store should know localhost after receiving STS header"
    );

    // 2. Visit /redirect — server responds 302 to http://localhost:{port}/final.
    //    The client must HSTS-upgrade the redirect target back to https://.
    let resp = client
        .get(&format!("https://localhost:{}/redirect", tls_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        "hsts-ok",
        "redirect target should have been HSTS-upgraded to HTTPS and reached the TLS server"
    );
}
