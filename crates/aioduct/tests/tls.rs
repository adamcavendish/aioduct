#![cfg(feature = "tokio")]

mod common;
use common::*;

#[derive(Clone)]
struct TokioExec;
impl<F> hyper::rt::Executor<F> for TokioExec
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn test_https_local_tls_server() {
    use std::sync::Arc;

    install_rustls_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let mut server_tls_config =
        rustls::ServerConfig::builder_with_provider(rustls_crypto_provider())
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
    let mut client_tls_config =
        rustls::ClientConfig::builder_with_provider(rustls_crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
    client_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config));

    let client: Client<TokioRuntime> = Client::builder()
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

    install_rustls_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    // Server only offers h1
    let mut server_tls_config =
        rustls::ServerConfig::builder_with_provider(rustls_crypto_provider())
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
    let mut client_tls_config =
        rustls::ClientConfig::builder_with_provider(rustls_crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
    client_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config));

    let client: Client<TokioRuntime> = Client::builder()
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

    install_rustls_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    // Server with NO ALPN
    let server_tls_config = rustls::ServerConfig::builder_with_provider(rustls_crypto_provider())
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
    let mut client_tls_config =
        rustls::ClientConfig::builder_with_provider(rustls_crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
    client_tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_tls_config));

    let client: Client<TokioRuntime> = Client::builder()
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

    install_rustls_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let mut server_tls_config =
        rustls::ServerConfig::builder_with_provider(rustls_crypto_provider())
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

    let client: Client<TokioRuntime> = Client::builder()
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
