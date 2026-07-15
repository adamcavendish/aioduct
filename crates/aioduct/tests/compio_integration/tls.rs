use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use hyper::service::service_fn;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use aioduct::HttpEngineLocal;
use aioduct::runtime::compio_rt::{CompioRuntime, TcpConnector};
use aioduct::tls::TlsVersion;

fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ── Cert generation helpers ──────────────────────────────────────

struct CaBundle {
    params: rcgen::CertificateParams,
    key: rcgen::KeyPair,
    cert_der: CertificateDer<'static>,
}

fn generate_ca() -> CaBundle {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let cert = params.self_signed(&key).unwrap();
    let cert_der = CertificateDer::from(cert.der().to_vec());
    CaBundle {
        params,
        key,
        cert_der,
    }
}

fn generate_signed_cert(
    ca: &CaBundle,
    domains: Vec<String>,
) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let params = rcgen::CertificateParams::new(domains).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca.params, &ca.key);
    let cert = params.signed_by(&key, &issuer).unwrap();
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(key.serialize_der().into());
    (cert_der, key_der)
}

fn generate_client_identity_pem(ca: &CaBundle) -> Vec<u8> {
    let key = rcgen::KeyPair::generate().unwrap();
    let params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca.params, &ca.key);
    let cert = params.signed_by(&key, &issuer).unwrap();
    let mut pem = cert.pem().into_bytes();
    pem.extend_from_slice(key.serialize_pem().as_bytes());
    pem
}

// ── Server starters ──────────────────────────────────────────────

fn make_server_config(
    cert_der: CertificateDer<'static>,
    key_der: PrivateKeyDer<'static>,
) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

fn start_tls_h1_server(
    cert_der: CertificateDer<'static>,
    key_der: PrivateKeyDer<'static>,
) -> SocketAddr {
    start_tls_h1_server_raw(make_server_config(cert_der, key_der), "hello compio tls")
}

fn start_tls_h1_server_raw(config: Arc<rustls::ServerConfig>, body: &'static str) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let acceptor = tokio_rustls::TlsAcceptor::from(config);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            io,
                            service_fn(move |_req| async move {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
                            }),
                        )
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

fn start_mtls_h1_server(
    ca_cert_der: CertificateDer<'static>,
    server_cert_der: CertificateDer<'static>,
    server_key_der: PrivateKeyDer<'static>,
) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut root_store = rustls::RootCertStore::empty();
            root_store.add(ca_cert_der).unwrap();
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(root_store),
                crypto_provider(),
            )
            .build()
            .unwrap();

            let mut config = rustls::ServerConfig::builder_with_provider(crypto_provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_client_cert_verifier(verifier)
                .with_single_cert(vec![server_cert_der], server_key_der)
                .unwrap();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let acceptor = acceptor.clone();
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
                                    "hello compio mtls",
                                ))))
                            }),
                        )
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

// ── Tests ────────────────────────────────────────────────────────

fn self_signed_cert() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    (cert_der, key_der)
}

fn url(addr: SocketAddr) -> String {
    format!("https://{addr}/")
}

fn make_tls13_only_server_config(
    cert_der: CertificateDer<'static>,
    key_der: PrivateKeyDer<'static>,
) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

// ── Tests ────────────────────────────────────────────────────────

#[test]
fn test_compio_unknown_selected_alpn_is_rejected_before_http_bytes() {
    install_crypto();
    let (cert_der, key_der) = self_signed_cert();
    let mut server_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    server_config.alpn_protocols = vec![b"custom-proto".to_vec()];
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt as _;

            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            let mut byte = [0_u8; 1];
            let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
            read_tx.send(read).unwrap();
        });
    });
    let addr = addr_rx.recv().unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).unwrap();
    let mut client_config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_config.alpn_protocols = vec![b"custom-proto".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_config));

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let error = client
            .get_local(&url(addr))
            .unwrap()
            .send()
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("custom-proto"),
            "unexpected unknown-ALPN error: {error}"
        );
    });

    assert_eq!(
        read_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
        0,
        "local dispatch sent HTTP bytes after negotiating an unknown ALPN"
    );
}

#[test]
fn test_compio_tls_prebuilt_connector() {
    install_crypto();
    let (cert_der, key_der) = self_signed_cert();
    let addr = start_tls_h1_server(cert_der.clone(), key_der);

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der).unwrap();
    let mut client_config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    client_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(Arc::new(client_config));

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello compio tls");
    });
}

#[test]
fn test_compio_tls_extra_root_certs() {
    install_crypto();
    let (cert_der, key_der) = self_signed_cert();
    let addr = start_tls_h1_server(cert_der.clone(), key_der);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let root_cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[root_cert])
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello compio tls");
    });
}

#[test]
fn test_compio_tls_version_constraint_ok() {
    install_crypto();
    let (cert_der, key_der) = self_signed_cert();
    let config = make_tls13_only_server_config(cert_der.clone(), key_der);
    let addr = start_tls_h1_server_raw(config, "hello compio tls");

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let root_cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[root_cert])
            .min_tls_version(TlsVersion::Tls1_3)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_tls_version_constraint_mismatch() {
    install_crypto();
    let (cert_der, key_der) = self_signed_cert();
    let config = make_tls13_only_server_config(cert_der.clone(), key_der);
    let addr = start_tls_h1_server_raw(config, "hello compio tls");

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let root_cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[root_cert])
            .max_tls_version(TlsVersion::Tls1_2)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let result = client.get_local(&url(addr)).unwrap().send().await;

        assert!(result.is_err(), "TLS version mismatch should cause error");
    });
}

#[test]
fn test_compio_tls_sni_disabled() {
    install_crypto();
    let (cert_der, key_der) = self_signed_cert();
    let addr = start_tls_h1_server(cert_der.clone(), key_der);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let root_cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[root_cert])
            .tls_sni(false)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello compio tls");
    });
}

#[test]
fn test_compio_tls_invalid_hostnames() {
    install_crypto();
    // Cert is for "wrong-host.example.com", but we connect to 127.0.0.1
    let cert = rcgen::generate_simple_self_signed(vec!["wrong-host.example.com".into()]).unwrap();
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let addr = start_tls_h1_server(cert_der.clone(), key_der);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let root_cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[root_cert])
            .danger_accept_invalid_hostnames(true)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello compio tls");
    });
}

#[test]
fn test_compio_tls_identity_mtls() {
    install_crypto();
    let ca = generate_ca();
    let (server_cert, server_key) = generate_signed_cert(&ca, vec!["127.0.0.1".into()]);
    let client_pem = generate_client_identity_pem(&ca);

    let addr = start_mtls_h1_server(ca.cert_der.clone(), server_cert, server_key);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let ca_cert = aioduct::tls::Certificate::from_der(ca.cert_der.to_vec());
        let identity = aioduct::tls::Identity::from_pem(&client_pem).unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[ca_cert])
            .identity(identity)
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello compio mtls");
    });
}

#[test]
fn test_compio_tls_hsts_store_from_https_response() {
    install_crypto();
    let (cert_der, key_der) = self_signed_cert();
    // Build a TLS server that returns Strict-Transport-Security header
    let config = make_server_config(cert_der.clone(), key_der);
    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let acceptor = tokio_rustls::TlsAcceptor::from(config);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr).unwrap();

                loop {
                    let (stream, _) = match listener.accept().await {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let acceptor = acceptor.clone();
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
                                    Ok::<_, Infallible>(
                                        Response::builder()
                                            .header(
                                                "strict-transport-security",
                                                "max-age=31536000; includeSubDomains",
                                            )
                                            .body(Full::new(Bytes::from("hsts ok")))
                                            .unwrap(),
                                    )
                                }),
                            )
                            .await;
                    });
                }
            });
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let root_cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
        let hsts = aioduct::hsts::HstsStore::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[root_cert])
            .hsts(hsts.clone())
            .timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();

        let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hsts ok");

        // HSTS should be stored because this was an HTTPS response
        assert!(
            hsts.should_upgrade("127.0.0.1"),
            "HSTS should be stored from HTTPS responses with STS header"
        );
    });
}
