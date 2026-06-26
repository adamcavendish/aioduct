#![cfg(feature = "tokio")]
#![cfg(feature = "rustls")]

#[path = "tls/local_servers.rs"]
mod local_servers;
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

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .hsts(hsts_store.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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

// RFC 7231 §5.5.2: a user agent MUST NOT send a Referer header in an
// unsecured HTTP request if the referring page was transferred with a secure
// protocol. This is the real HTTPS→HTTP downgrade test that the placeholder in
// redirects.rs could not express without TLS. The origin is a genuine HTTPS
// server (trusted self-signed cert) that 302-redirects to a plaintext HTTP
// target; with referer(true) the HTTP target must still receive NO Referer.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn referer_not_leaked_on_https_to_http_downgrade() {
    use std::sync::{Arc, Mutex};

    install_crypto_provider();

    // Plain HTTP target: captures whatever Referer header it receives.
    let captured_referer: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap = captured_referer.clone();
    let (http_addr, _http_counter) = aioduct_test_server::h1::h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            *cap.lock().unwrap() = req
                .headers()
                .get("referer")
                .map(|v| v.to_str().unwrap_or("").to_string());
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("http-target"))))
        }
    })
    .await;

    // HTTPS origin: 302-redirects to the plaintext HTTP target.
    let (tls_addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], move |_req| {
            let target = format!("http://{http_addr}/landing");
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

    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!(
            "https://localhost:{}/secret-path",
            tls_addr.port()
        ))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "http-target");

    let referer = captured_referer.lock().unwrap().clone();
    assert!(
        referer.is_none(),
        "RFC 7231 §5.5.2: Referer MUST NOT be sent on an HTTPS→HTTP downgrade, \
         but the HTTP target received: {referer:?}"
    );
}

// ── TLS Integration Tests ─────────────────────────────────────────────

// Test 1: custom CA trusts end-to-end.
// Generate a custom CA, issue a server cert signed by it, start a TLS
// server, and connect using RustlsConnector::with_extra_roots.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn custom_ca_trusts_end_to_end() {
    use std::sync::Arc;

    install_crypto_provider();

    // Generate CA
    let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .key_usages
        .push(rcgen::KeyUsagePurpose::KeyCertSign);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by the custom CA
    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    server_params.is_ca = rcgen::IsCa::NoCa;
    let server_key = rcgen::KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();
    let server_cert_der = rustls::pki_types::CertificateDer::from(server_cert.der().to_vec());
    let server_key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(server_key.serialize_der().into());

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![server_cert_der], server_key_der)
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
                                    "custom ca ok",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    // Client trusts the custom CA via with_extra_roots
    let extra_cert = aioduct::Certificate::from_der(ca_cert_der.to_vec());
    let connector = aioduct::tls::RustlsConnector::with_extra_roots(&[extra_cert]);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
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
    let body = resp.text().await.unwrap();
    assert_eq!(body, "custom ca ok");
}

// Test 2: TLS 1.3 client rejects a TLS 1.2-only server.
// Client uses with_webpki_roots_versioned(TLSv13). Server only offers
// TLS 1.2. The version mismatch causes a connect error.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls13_client_rejects_tls12_server() {
    use std::sync::Arc;

    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let client_cert = aioduct::tls::Certificate::from_der(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    // Server: TLS 1.2 only
    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_protocol_versions(&[&rustls::version::TLS12])
        .expect("configured rustls provider does not support TLS 1.2")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
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
                                    "should not reach",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    // Client: TLS 1.3 only, with server cert trusted so cert verification
    // cannot mask a missing version restriction.
    let connector = aioduct::tls::RustlsConnector::with_extra_roots_versioned(
        &[client_cert],
        &[&rustls::version::TLS13],
    );

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await;

    match resp {
        Err(e) => {
            assert!(
                e.is_connect(),
                "version rejection error must be a connect error, got: {e:?}"
            );
        }
        Ok(r) => panic!("expected error, got status {}", r.status()),
    }
}

// Test 3: mTLS — client without identity fails against a server that
// requires client authentication.
//
// The test-server helpers do not expose a `with_client_auth` API, so the
// server is built inline using WebPkiClientVerifier (without
// allow_unauthenticated) to require a client certificate.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn mutual_tls_client_no_identity_server_requires() {
    use std::sync::Arc;

    install_crypto_provider();

    // Generate a CA for signing the server cert
    let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .key_usages
        .push(rcgen::KeyUsagePurpose::KeyCertSign);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by the CA
    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    server_params.is_ca = rcgen::IsCa::NoCa;
    let server_key = rcgen::KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();
    let server_cert_der = rustls::pki_types::CertificateDer::from(server_cert.der().to_vec());
    let server_key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(server_key.serialize_der().into());

    // Dummy CA root so the WebPkiClientVerifier builder has at least one
    // trusted root (the builder rejects an empty store). The actual client
    // has no identity, so no client cert will be presented.
    let dummy_root = rcgen::generate_simple_self_signed(vec!["dummy-ca".into()]).unwrap();
    let mut client_root_store = rustls::RootCertStore::empty();
    client_root_store
        .add(rustls::pki_types::CertificateDer::from(
            dummy_root.cert.der().to_vec(),
        ))
        .unwrap();
    let client_verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(client_root_store),
        crypto_provider(),
    )
    // No .allow_unauthenticated() — client auth is REQUIRED.
    .build()
    .unwrap();

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(vec![server_cert_der], server_key_der)
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
                                    "should not reach",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    // Client: trusts the CA (so server cert is valid) but has no identity
    let ca_cert_aioduct = aioduct::Certificate::from_der(ca_cert_der.to_vec());
    let connector = aioduct::tls::RustlsConnector::with_extra_roots(&[ca_cert_aioduct]);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await;

    match resp {
        Err(e) => {
            // The mTLS handshake fails with a TLS alert (CertificateRequired)
            // wrapped in a hyper error. Verify the connection was rejected.
            let msg = format!("{e}");
            assert!(
                !msg.contains("timeout"),
                "mTLS failure should not be a timeout: {e}"
            );
        }
        Ok(r) => panic!("expected mTLS handshake failure, got status {}", r.status()),
    }
}

// ── SNI disable test ───────────────────────────────────────────────────

/// Verifies that when `tls_sni(false)` is set on the builder, the server
/// does NOT receive an SNI extension. A second connection with the default
/// SNI (enabled) confirms the server sees `Some("localhost")`.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_sni_disabled_server_does_not_receive_sni() {
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

    let (sni_tx, mut sni_rx) = tokio::sync::mpsc::unbounded_channel();

    tokio::spawn({
        let tls_acceptor = tls_acceptor.clone();
        async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let acceptor = tls_acceptor.clone();
                let sni_tx = sni_tx.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    // Record the SNI as seen by the server
                    let sni = tls_stream.get_ref().1.server_name().map(|s| s.to_string());
                    let _ = sni_tx.send(sni);

                    let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);
                    let _ = hyper::server::conn::http2::Builder::new(TokioExec)
                        .serve_connection(
                            io,
                            service_fn(|_req| async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    // Shared client TLS config that trusts the server cert
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der).unwrap();
    let mut client_tls_config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_root_certificates(root_store)
        .with_no_client_auth();
    client_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    // Connection 1: SNI disabled
    {
        let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config.clone()));
        let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
            .tls(connector)
            .tls_sni(false)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let _ = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        let first_sni = sni_rx
            .recv()
            .await
            .expect("channel closed before SNI received");
        assert!(
            first_sni.is_none(),
            "SNI should be None when tls_sni is disabled, got: {first_sni:?}"
        );
    }

    // Connection 2: SNI enabled (default behavior)
    {
        let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config));
        let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
            .tls(connector)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();

        let _ = client
            .get(&format!("https://localhost:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        let second_sni = sni_rx
            .recv()
            .await
            .expect("channel closed before SNI received");
        assert_eq!(
            second_sni,
            Some("localhost".to_string()),
            "SNI should be Some(\"localhost\") with default SNI enabled"
        );
    }
}

// ── ALPN mismatch test ─────────────────────────────────────────────────

/// Client offers only h2 via ALPN; server only offers http/1.1. The rustls
/// server sends a fatal NoApplicationProtocol alert, so the connection must
/// fail. Uses a custom rustls config (h2-only ALPN), not the default
/// [h2, http/1.1].
#[cfg(feature = "rustls")]
#[tokio::test]
async fn alpn_h2_only_client_rejects_h1_server() {
    use std::sync::Arc;

    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    // Server: only http/1.1 ALPN
    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
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
                                    "should not reach",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    // Client: h2-only ALPN (custom config, NOT default [h2, http/1.1])
    let mut connector = aioduct::tls::RustlsConnector::danger_accept_invalid_certs();
    connector.config_mut().alpn_protocols = vec![b"h2".to_vec()];

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await;

    match resp {
        Err(e) => {
            assert!(
                e.is_connect(),
                "ALPN mismatch error must be a connect error, got: {e:?}"
            );
        }
        Ok(_) => panic!("expected ALPN mismatch to cause connection failure"),
    }
}

// ── danger_accept_invalid_certs smoke test ──────────────────────────────

/// Verifies that `danger_accept_invalid_certs()` (NoVerifier) bypasses ALL
/// certificate verification and allows a connection to a self-signed server
/// to succeed with a 200 OK response.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn danger_accept_invalid_certs_allows_self_signed() {
    use std::sync::Arc;

    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
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
                                    "insecure ok",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        }
    });

    let connector = aioduct::tls::RustlsConnector::danger_accept_invalid_certs();

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

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
                "danger_accept_invalid_certs request timed out: {e}"
            );
            panic!("danger_accept_invalid_certs request failed: {e}");
        }
    }
    let body = resp.unwrap().text().await.unwrap();
    assert_eq!(body, "insecure ok");
}

// Sensitive headers (Authorization, Cookie) must be stripped when a redirect
// downgrades from HTTPS to HTTP, even when the host and port are the same.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn sensitive_headers_stripped_on_https_to_http_downgrade() {
    use std::sync::{Arc, Mutex};

    install_crypto_provider();

    #[derive(Default, Clone)]
    struct CapturedHeaders {
        authorization: Option<String>,
        cookie: Option<String>,
    }

    let captured: Arc<Mutex<CapturedHeaders>> = Arc::new(Mutex::new(CapturedHeaders::default()));
    let cap = captured.clone();
    let (http_addr, _http_counter) = aioduct_test_server::h1::h1_server_with(move |req| {
        let cap = cap.clone();
        async move {
            let mut h = cap.lock().unwrap();
            h.authorization = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap_or("").to_string());
            h.cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_string());
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("landed"))))
        }
    })
    .await;

    // HTTPS origin: 302-redirects to the plaintext HTTP target.
    let (tls_addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], move |_req| {
            let target = format!("http://{http_addr}/landing");
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

    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/secret", tls_addr.port()))
        .unwrap()
        .header(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer secret-token"),
        )
        .header(
            http::header::COOKIE,
            http::HeaderValue::from_static("session=abc123"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "landed");

    let h = captured.lock().unwrap();
    assert!(
        h.authorization.is_none(),
        "Authorization MUST be stripped on HTTPS→HTTP downgrade, \
         but the HTTP target received: {:?}",
        h.authorization
    );
    assert!(
        h.cookie.is_none(),
        "Cookie MUST be stripped on HTTPS→HTTP downgrade, \
         but the HTTP target received: {:?}",
        h.cookie
    );
}
