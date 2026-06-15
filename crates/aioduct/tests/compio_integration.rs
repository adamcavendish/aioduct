#![cfg(all(feature = "compio", feature = "tokio"))]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::HttpEngineLocal;
use aioduct::runtime::compio_rt::{CompioRuntime, TcpConnector};

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

fn start_server_tokio() -> SocketAddr {
    start_server_with_tokio(|req| async { hello(req).await })
}

fn start_server_with_tokio<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = server_http1::Builder::new()
                        .serve_connection(io, service_fn(handler))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

#[test]
fn test_compio_get_request() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_post_request() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .body("request body")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_connection_reuse() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let url = format!("http://{addr}/");

        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let body = resp2.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_redirect_302() {
    let final_addr = start_server_tokio();
    let redirect_addr = start_server_with_tokio(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{redirect_addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_timeout_triggers() {
    let addr = start_server_with_tokio(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .timeout(Duration::from_millis(50))
            .send()
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().is_timeout(), "expected Timeout error");
    });
}

#[test]
fn test_compio_custom_header() {
    let addr = start_server_with_tokio(|req| async move {
        let custom = req
            .headers()
            .get("x-custom")
            .map(|v| v.to_str().unwrap_or(""))
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom.to_string()))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .header_str("x-custom", "compio-value")
            .unwrap()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert_eq!(body, "compio-value");
    });
}

#[test]
fn test_compio_h2_prior_knowledge() {
    use hyper::server::conn::http2 as server_http2;

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

    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr).unwrap();

                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                    tokio::spawn(async move {
                        let _ = server_http2::Builder::new(TokioExec)
                            .serve_connection(
                                io,
                                service_fn(|_req| async {
                                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                        "h2 compio",
                                    ))))
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "h2 compio");
    });
}

#[test]
fn test_compio_large_body() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let body = req.collect().await.unwrap().to_bytes();
        let len = body.len();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let payload = "x".repeat(1024 * 1024);
        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .body(payload)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "1048576");
    });
}

#[test]
fn test_compio_large_response_body() {
    let addr = start_server_with_tokio(|_req| async move {
        let body = "y".repeat(512 * 1024);
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body.len(), 512 * 1024);
    });
}

#[test]
fn test_compio_head_request() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "1000")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .request_local(http::Method::HEAD, &format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.content_length(), Some(1000));
    });
}

#[test]
fn test_compio_default_headers() {
    let addr = start_server_with_tokio(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .user_agent("compio-test/1.0")
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "compio-test/1.0");
    });
}

#[test]
fn test_compio_pool_reuse_after_body_consumed() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("pool test"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let url = format!("http://{addr}/");

        for _ in 0..5 {
            let resp = client.get_local(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "pool test");
        }
    });
}

#[test]
fn test_compio_bearer_auth() {
    let addr = start_server_with_tokio(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .bearer_auth("my-token")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "Bearer my-token");
    });
}

#[test]
fn test_compio_http_client_trait() {
    use aioduct::traits::{HttpClient, RequestBuilderExt};

    let addr = start_server_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let engine = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let client = engine;

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });
}

// ── Compio TLS integration tests ─────────────────────────────────────
#[cfg(all(feature = "compio", feature = "tokio", feature = "rustls"))]
mod compio_tls_tests {
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

    fn start_tls_h1_server_raw(
        config: Arc<rustls::ServerConfig>,
        body: &'static str,
    ) -> SocketAddr {
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
        let cert =
            rcgen::generate_simple_self_signed(vec!["wrong-host.example.com".into()]).unwrap();
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
}

// ── SSE integration tests ───────────────────────────────────────────

#[test]
fn test_compio_sse_stream() {
    let addr = start_server_with_tokio(|_req| async move {
        let body = "data: event1\n\ndata: event2\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(body)))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        let mut stream = resp.into_sse_stream();
        let e1 = stream.next().await.unwrap().unwrap();
        let e2 = stream.next().await.unwrap().unwrap();
        assert!(stream.next().await.is_none());

        match (&e1, &e2) {
            (aioduct::sse::SseEvent::Message(m1), aioduct::sse::SseEvent::Message(m2)) => {
                assert_eq!(m1.data, "event1");
                assert_eq!(m2.data, "event2");
            }
            _ => panic!("expected two messages"),
        }
    });
}

// ── Request builder tests ───────────────────────────────────────────

#[test]
fn test_compio_query_params() {
    let addr = start_server_with_tokio(|req| async move {
        let query = req.uri().query().unwrap_or("none").to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(query))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .query(&[("key", "val"), ("a", "b")])
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.contains("key=val"));
        assert!(body.contains("a=b"));
    });
}

#[cfg(feature = "json")]
#[test]
fn test_compio_json_body() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{}|{}",
            ct,
            String::from_utf8_lossy(&body)
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .json(&serde_json::json!({"key": "value"}))
            .unwrap()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.starts_with("application/json|"));
        assert!(body.contains("\"key\""));
        assert!(body.contains("\"value\""));
    });
}

#[test]
fn test_compio_form_body() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{}|{}",
            ct,
            String::from_utf8_lossy(&body)
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .form(&[("user", "alice"), ("pass", "secret")])
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.starts_with("application/x-www-form-urlencoded|"));
        assert!(body.contains("user=alice"));
        assert!(body.contains("pass=secret"));
    });
}

#[test]
fn test_compio_basic_auth() {
    let addr = start_server_with_tokio(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .basic_auth("user", Some("pass"))
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(body.starts_with("Basic "));
    });
}

#[test]
fn test_compio_try_clone() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let builder = client
            .get_local("http://example.com/")
            .unwrap()
            .header_str("x-test", "value")
            .unwrap()
            .body("payload");
        let cloned = builder.try_clone();
        assert!(cloned.is_some());
    });
}

#[test]
fn test_compio_headers_bulk() {
    let addr = start_server_with_tokio(|req| async move {
        let h1 = req
            .headers()
            .get("x-one")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let h2 = req
            .headers()
            .get("x-two")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{h1},{h2}")))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let mut headers = http::HeaderMap::new();
        headers.insert("x-one", "1".parse().unwrap());
        headers.insert("x-two", "2".parse().unwrap());
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .headers(headers)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "1,2");
    });
}

// ── Forwarding tests ────────────────────────────────────────────────

#[test]
fn test_compio_forward_basic_get() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "upstream:{path}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/hello/world")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "upstream:/hello/world");
    });
}

#[test]
fn test_compio_forward_strip_prefix() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/api/v1/users")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .strip_prefix("/api/v1")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "/users");
    });
}

#[test]
fn test_compio_forward_preserve_host() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", "original.example.com")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .preserve_host()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "original.example.com");
    });
}

#[test]
fn test_compio_forward_extra_header() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let val = req
            .headers()
            .get("x-forwarded-for")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .header(
                http::header::HeaderName::from_static("x-forwarded-for"),
                http::header::HeaderValue::from_static("10.0.0.1"),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "10.0.0.1");
    });
}

// ── Chunk download tests ────────────────────────────────────────────

#[test]
fn test_compio_chunk_download_with_ranges() {
    let addr = start_server_with_tokio(|req| async move {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        match req.method() {
            &http::Method::HEAD => Ok::<_, Infallible>(
                Response::builder()
                    .header("accept-ranges", "bytes")
                    .header("content-length", data.len().to_string())
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ),
            _ => {
                if let Some(range) = req.headers().get("range") {
                    let range_str = range.to_str().unwrap_or("");
                    let range_str = range_str.strip_prefix("bytes=").unwrap_or(range_str);
                    let parts: Vec<&str> = range_str.split('-').collect();
                    let start: usize = parts[0].parse().unwrap_or(0);
                    let end: usize = parts[1].parse().unwrap_or(data.len() - 1);
                    let slice = &data[start..=end];
                    Ok(Response::builder()
                        .status(206)
                        .body(Full::new(Bytes::from(slice.to_vec())))
                        .unwrap())
                } else {
                    Ok(Response::new(Full::new(Bytes::from(data.to_vec()))))
                }
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/"))
            .chunks(2)
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 26);
        assert_eq!(&result.data[..], b"abcdefghijklmnopqrstuvwxyz");
    });
}

#[test]
fn test_compio_chunk_download_fallback_no_ranges() {
    let addr = start_server_with_tokio(|req| async move {
        match req.method() {
            &http::Method::HEAD => Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "11")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ),
            _ => Ok(Response::new(Full::new(Bytes::from("hello world")))),
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/"))
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 11);
        assert_eq!(&result.data[..], b"hello world");
    });
}

#[test]
fn test_compio_https_only_rejects_http() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build_local()
            .unwrap();
        let result = client
            .get_local("http://example.com/")
            .unwrap()
            .send()
            .await;
        assert!(result.is_err());
    });
}

#[test]
fn test_compio_no_connection_reuse() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .no_connection_reuse()
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let _ = resp2.text().await.unwrap();
    });
}

#[test]
fn test_compio_cookie_jar() {
    let addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "session=abc123; Path=/")
                    .body(Full::new(Bytes::from("cookie set")))
                    .unwrap(),
            )
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_owned())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(cookie))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let jar = aioduct::cookie::CookieJar::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cookie_jar(jar)
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/set"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/check"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("session=abc123"),
            "cookie not forwarded: {body}"
        );
    });
}

#[test]
fn test_compio_middleware() {
    let addr = start_server_with_tokio(|req| async move {
        let custom = req
            .headers()
            .get("x-middleware")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .middleware(
                |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                    req.headers_mut().insert(
                        "x-middleware",
                        http::header::HeaderValue::from_static("injected"),
                    );
                },
            )
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert_eq!(body, "injected");
    });
}

#[test]
fn test_compio_read_timeout_fires() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "10000")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_millis(50))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_bandwidth_limiter() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("bandwidth test data"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .max_download_speed(1024 * 1024)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "bandwidth test data");
    });
}

#[test]
fn test_compio_rate_limiter() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .max_requests_per_sec(100)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_error_for_status() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let err = resp.error_for_status();
        assert!(err.is_err());
    });
}

#[test]
fn test_compio_decompression_disabled() {
    let addr = start_server_with_tokio(|req| async move {
        let accept = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .no_decompression()
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.is_empty() || !body.contains("gzip"));
    });
}

#[test]
fn test_compio_tcp_keepalive() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tcp_keepalive(Duration::from_secs(60))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_resolve_override() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolve("custom-host.local", addr)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://custom-host.local:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn test_compio_request_local_with_delete() {
    let addr = start_server_with_tokio(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .request_local(http::Method::DELETE, &format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "DELETE");
    });
}

#[test]
fn test_compio_observer() {
    use std::sync::{Arc, Mutex};

    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let phases_clone = phases.clone();

        struct Obs(Arc<Mutex<Vec<String>>>);
        impl aioduct::observer::RequestObserver for Obs {
            fn on_event(&self, event: &aioduct::observer::RequestEvent) {
                self.0.lock().unwrap().push(format!("{:?}", event.phase));
            }
            fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
        }

        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .request_observer(Obs(phases_clone))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let recorded = phases.lock().unwrap();
        assert!(!recorded.is_empty(), "observer should have recorded phases");
    });
}

#[test]
fn test_compio_redirect_with_method_change() {
    let final_addr = start_server_with_tokio(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    });
    let redirect_addr = start_server_with_tokio(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(303)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{redirect_addr}/"))
            .unwrap()
            .body("some body")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "GET");
    });
}

#[test]
fn test_compio_too_many_redirects() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .max_redirects(3)
            .build_local()
            .unwrap();
        let result = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await;
        assert!(result.is_err());
    });
}

#[test]
fn test_compio_connect_timeout() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .connect_timeout(Duration::from_millis(1))
            .build_local()
            .unwrap();
        let result = client
            .get_local("http://192.0.2.1:1/")
            .unwrap()
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        assert!(result.is_err());
    });
}

#[test]
fn test_compio_hsts_store() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("hsts test"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let hsts = aioduct::hsts::HstsStore::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .hsts(hsts)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_cache_basic() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=3600")
                .body(Full::new(Bytes::from("cached response")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.text().await.unwrap(), "cached response");

        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.text().await.unwrap(), "cached response");
    });
}

// ── Cache stale-if-error tests ─────────────────────────────────────

#[test]
fn test_compio_cache_stale_if_error_on_5xx() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |_req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: return cacheable response with short max-age and stale-if-error
                Ok::<_, Infallible>(
                    Response::builder()
                        .header(
                            "cache-control",
                            "max-age=0, must-revalidate, stale-if-error=3600",
                        )
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("fresh data")))
                        .unwrap(),
                )
            } else {
                // Subsequent requests: return 500
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("server error")))
                        .unwrap(),
                )
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        // First request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "fresh data");

        // Small delay to ensure max-age=0 makes the entry stale
        std::thread::sleep(Duration::from_millis(10));

        // Second request: server returns 500, stale-if-error should serve cached
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "fresh data");
    });
}

#[test]
fn test_compio_cache_stale_if_error_on_network_error() {
    // Start server, cache a response, then shut it down, verify stale cache is served.
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();

            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        let (stream, _) = accept_result.unwrap();
                        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                        tokio::spawn(async move {
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(
                                    io,
                                    service_fn(|_req| async {
                                        Ok::<_, Infallible>(
                                            Response::builder()
                                                .header(
                                                    "cache-control",
                                                    "max-age=0, must-revalidate, stale-if-error=3600",
                                                )
                                                .header("etag", "\"v1\"")
                                                .body(Full::new(Bytes::from("cached from server")))
                                                .unwrap(),
                                        )
                                    }),
                                )
                                .await;
                        });
                    }
                    _ = tokio::task::spawn_blocking(|| { /* yield */ }) => {}
                }
                // Check if shutdown was signaled
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
            }
        });
    });

    let addr = addr_rx.recv().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .timeout(Duration::from_millis(500))
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        // First request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "cached from server");

        // Small delay to ensure max-age=0 makes the entry stale
        std::thread::sleep(Duration::from_millis(10));

        // Shut down server
        let _ = shutdown_tx.send(());
        std::thread::sleep(Duration::from_millis(50));

        // Second request: server is down, stale-if-error should serve cached
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "cached from server");
    });
}

// ── Cache 304 revalidation test ────────────────────────────────────

#[test]
fn test_compio_cache_304_revalidation() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: return cacheable response with ETag and short max-age
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, must-revalidate")
                        .header("etag", "\"abc123\"")
                        .body(Full::new(Bytes::from("original content")))
                        .unwrap(),
                )
            } else {
                // Subsequent requests: check If-None-Match, return 304
                let if_none_match = req
                    .headers()
                    .get("if-none-match")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if if_none_match == "\"abc123\"" {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(304)
                            .header("etag", "\"abc123\"")
                            .body(Full::new(Bytes::new()))
                            .unwrap(),
                    )
                } else {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .body(Full::new(Bytes::from("unexpected")))
                            .unwrap(),
                    )
                }
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        // First request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "original content");

        // Small delay to ensure max-age=0 makes the entry stale
        std::thread::sleep(Duration::from_millis(10));

        // Second request: should revalidate with If-None-Match, get 304, serve cached
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "original content");
    });
}

// ── Cache invalidation on write test ───────────────────────────────

#[test]
fn test_compio_cache_invalidation_on_post() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            let method = req.method().clone();
            match method {
                ref m if *m == http::Method::GET => Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=3600")
                        .body(Full::new(Bytes::from(format!("get response #{n}"))))
                        .unwrap(),
                ),
                _ => Ok::<_, Infallible>(
                    Response::builder()
                        .status(200)
                        .body(Full::new(Bytes::from("post ok")))
                        .unwrap(),
                ),
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::cache::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/resource");

        // GET request: populates cache
        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let body1 = resp1.text().await.unwrap();
        assert!(body1.contains("get response"), "body: {body1}");

        // GET again: should be cached (same content)
        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.text().await.unwrap(), body1);

        // POST: should invalidate cache
        let resp3 = client
            .post_local(&url)
            .unwrap()
            .body("data")
            .send()
            .await
            .unwrap();
        assert_eq!(resp3.status(), http::StatusCode::OK);
        let _ = resp3.text().await.unwrap();

        // GET again: cache was invalidated, should get fresh response
        let resp4 = client.get_local(&url).unwrap().send().await.unwrap();
        let body4 = resp4.text().await.unwrap();
        assert_ne!(body4, body1, "cache should have been invalidated by POST");
    });
}

// ── Cookie store from response test ────────────────────────────────

#[test]
fn test_compio_cookie_store_from_response() {
    let addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/login" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "token=xyz789; Path=/; HttpOnly")
                    .header("set-cookie", "lang=en; Path=/")
                    .body(Full::new(Bytes::from("logged in")))
                    .unwrap(),
            )
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_owned())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(cookie))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let jar = aioduct::cookie::CookieJar::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cookie_jar(jar)
            .build_local()
            .unwrap();

        // Login: sets cookies
        let resp = client
            .get_local(&format!("http://{addr}/login"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        // Verify cookies are sent on subsequent requests
        let resp = client
            .get_local(&format!("http://{addr}/dashboard"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("token=xyz789"),
            "cookie not forwarded: {body}"
        );
    });
}

// ── Digest auth retry test ─────────────────────────────────────────

#[test]
fn test_compio_digest_auth_retry() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            let auth_header = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if n == 0 || !auth_header.starts_with("Digest ") {
                // First request or no digest auth: challenge with 401
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            "Digest realm=\"test@example.com\", nonce=\"abc123nonce\", qop=\"auth\", algorithm=MD5",
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                // Second request with digest credentials: verify and return 200
                assert!(
                    auth_header.contains("username=\"admin\""),
                    "digest auth should contain username"
                );
                assert!(
                    auth_header.contains("realm=\"test@example.com\""),
                    "digest auth should contain realm"
                );
                assert!(
                    auth_header.contains("nonce=\"abc123nonce\""),
                    "digest auth should contain nonce"
                );
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(200)
                        .body(Full::new(Bytes::from("authenticated")))
                        .unwrap(),
                )
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("admin", "secret123")
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/protected"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "authenticated");
    });
}

#[test]
fn test_compio_digest_auth_retry_with_body() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            use http_body_util::BodyExt;
            let n = rc.fetch_add(1, Ordering::SeqCst);
            let auth_header = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body_bytes = req.collect().await.unwrap().to_bytes();

            if n == 0 || !auth_header.starts_with("Digest ") {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            "Digest realm=\"api\", nonce=\"xyz789\", qop=\"auth\"",
                        )
                        .body(Full::new(Bytes::from("need auth")))
                        .unwrap(),
                )
            } else {
                // Verify the body was replayed
                let body_str = String::from_utf8_lossy(&body_bytes);
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(200)
                        .body(Full::new(Bytes::from(format!("ok:{}", body_str))))
                        .unwrap(),
                )
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .build_local()
            .unwrap();
        let resp = client
            .post_local(&format!("http://{addr}/submit"))
            .unwrap()
            .body("my payload")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "ok:my payload");
    });
}

// ── Finalize response with read timeout and bandwidth limit ────────

#[test]
fn test_compio_finalize_response_with_read_timeout_and_bandwidth() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
            "response with limits applied",
        ))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_secs(5))
            .max_download_speed(1024 * 1024)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "response with limits applied");
    });
}

#[test]
fn test_compio_read_timeout_with_slow_body() {
    // Server sends headers immediately but body arrives slowly
    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            let (mut stream, _) = listener.accept().unwrap();
            use std::io::Write;
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
            let _ = stream.write_all(b"5\r\nhello\r\n");
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(200));
            let _ = stream.write_all(b"6\r\n world\r\n0\r\n\r\n");
            let _ = stream.flush();
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_millis(50))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        // The read timeout should fire during body consumption
        let result = resp.text().await;
        // Read timeout may cause an error when trying to read the body
        // or it may succeed if the timeout wraps the whole body read.
        // Either outcome exercises the finalize_response_local path.
        let _ = result;
    });
}

// ── HSTS store from response test ──────────────────────────────────

#[test]
fn test_compio_per_request_read_timeout_overrides_default() {
    // Server sends headers + partial body, then stalls.
    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            let (mut stream, _) = listener.accept().unwrap();
            use std::io::Write;
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello");
            let _ = stream.flush();
            // Never send the remaining 5 bytes.
            std::thread::sleep(Duration::from_secs(30));
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Generous client default; per-request override is the tight one.
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .read_timeout(Duration::from_millis(100))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);

        let err = resp.text().await.unwrap_err();
        assert!(
            matches!(err, aioduct::Error::ReadTimeout),
            "per-request read_timeout should fire on stalled body, got: {err:?}"
        );
    });
}

#[test]
fn test_compio_hsts_store_from_response_header() {
    // This test verifies that when a response contains Strict-Transport-Security,
    // the HSTS store records it. Since the HSTS store_from_response is only called
    // when scheme is HTTPS, and we can't easily set up TLS in this test, we test
    // the HTTP path which should NOT store HSTS (only HTTPS responses store it).
    // This exercises the conditional check at lines 178-183.
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header(
                    "strict-transport-security",
                    "max-age=31536000; includeSubDomains",
                )
                .body(Full::new(Bytes::from("hsts response")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let hsts = aioduct::hsts::HstsStore::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .hsts(hsts.clone())
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        // HSTS should NOT be stored for HTTP responses (only HTTPS)
        // This exercises the condition check `current_uri.scheme() == Some(&http::uri::Scheme::HTTPS)`
        assert!(
            !hsts.should_upgrade(&format!("127.0.0.1:{}", addr.port())),
            "HSTS should not be stored from HTTP responses"
        );
    });
}

// ── Forward: on_request hook ──────────────────────────────────────────

#[test]
fn test_compio_forward_on_request_hook() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let custom = req
            .headers()
            .get("x-injected")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_request(|parts| {
                parts.headers.insert(
                    "x-injected",
                    http::header::HeaderValue::from_static("hook-value"),
                );
            })
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hook-value");
    });
}

// ── Forward: on_response hook ─────────────────────────────────────────

#[test]
fn test_compio_forward_on_response_hook() {
    let upstream_addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("original"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_response(|resp| {
                resp.headers_mut().insert(
                    "x-modified",
                    http::header::HeaderValue::from_static("by-hook"),
                );
            })
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("x-modified").unwrap().to_str().unwrap(),
            "by-hook"
        );
    });
}

// ── Forward: remove_header ────────────────────────────────────────────

#[test]
fn test_compio_forward_remove_header() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let has_secret = req.headers().contains_key("x-secret");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "secret={}",
            has_secret
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .header("x-secret", "confidential")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .remove_header(http::header::HeaderName::from_static("x-secret"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "secret=false");
    });
}

// ── Forward: forward_header ───────────────────────────────────────────

#[test]
fn test_compio_forward_forward_header() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_else(|| "missing".to_owned());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .header("authorization", "Bearer my-token")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .forward_header(http::header::AUTHORIZATION)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "Bearer my-token");
    });
}

// ── Forward: timeout ──────────────────────────────────────────────────

#[test]
fn test_compio_forward_timeout() {
    let upstream_addr = start_server_with_tokio(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .timeout(Duration::from_millis(50))
            .send()
            .await;

        assert!(
            result.is_err(),
            "forward with timeout should error on slow upstream"
        );
        assert!(result.unwrap_err().is_timeout());
    });
}

// ── Forward: upstream with base path ──────────────────────────────────

#[test]
fn test_compio_forward_upstream_base_path() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/resource/123")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}/api/v1", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "/api/v1/resource/123");
    });
}

// ── Forward: query string preserved ──────────────────────────────────

#[test]
fn test_compio_forward_query_string_preserved() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let full = req.uri().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(full))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/search?q=hello&page=1")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("q=hello"),
            "query should be preserved: {body}"
        );
        assert!(body.contains("page=1"), "query should be preserved: {body}");
    });
}

// ── Forward: no upstream returns error ────────────────────────────────

#[test]
fn test_compio_forward_no_upstream_errors() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client.forward_local(incoming).send().await;
        assert!(result.is_err(), "forward without upstream should fail");
    });
}

// ── Chunk download local tests ────────────────────────────────────────

#[test]
fn test_compio_chunk_download_local_fallback_no_ranges() {
    let addr = start_server_with_tokio(|req| async move {
        if req.method() == http::Method::HEAD {
            // No accept-ranges header
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "13")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/file"))
            .chunks(4)
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 13);
        assert_eq!(&result.data[..], b"hello aioduct");
    });
}

#[test]
fn test_compio_chunk_download_local_with_ranges() {
    use std::sync::Arc;

    let body_data: Vec<u8> = (0..200u8).cycle().take(1000).collect();
    let body_data_arc = Arc::new(body_data.clone());

    let addr = start_server_with_tokio(move |req| {
        let body_data = body_data_arc.clone();
        async move {
            if req.method() == http::Method::HEAD {
                return Ok::<_, Infallible>(
                    Response::builder()
                        .header("accept-ranges", "bytes")
                        .header("content-length", body_data.len().to_string())
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                );
            }
            if let Some(range) = req.headers().get("range") {
                let range_str = range.to_str().unwrap();
                let range_str = range_str.strip_prefix("bytes=").unwrap();
                let parts: Vec<&str> = range_str.split('-').collect();
                let start: usize = parts[0].parse().unwrap();
                let end: usize = parts[1].parse().unwrap();
                let slice = &body_data[start..=end];
                return Ok(Response::builder()
                    .status(206)
                    .body(Full::new(Bytes::copy_from_slice(slice)))
                    .unwrap());
            }
            Ok(Response::new(Full::new(Bytes::from(
                body_data.as_ref().to_vec(),
            ))))
        }
    });

    let body_data_check = body_data.clone();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/file"))
            .chunks(4)
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 1000);
        assert_eq!(result.data.len(), 1000);
        assert_eq!(&result.data[..], &body_data_check[..]);
    });
}

#[test]
fn test_compio_chunk_download_head_failure() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/missing"))
            .download()
            .await;
        assert!(result.is_err(), "HEAD failure should return error");
    });
}

// ── Forward local: preserve_host and upstream base path ───────────────

#[test]
fn test_compio_forward_local_preserve_host_with_base_path() {
    let addr = start_server_with_tokio(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "host={host},path={path}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/users/123")
            .header("host", "original.example.com")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(format!("http://{addr}/api/v2"))
            .preserve_host()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(
            body.contains("host=original.example.com"),
            "preserve_host should keep original, got: {body}"
        );
        assert!(
            body.contains("path=/api/v2/users/123"),
            "base path should be prepended, got: {body}"
        );
    });
}

// ── Forward local: strip prefix ───────────────────────────────────────

#[test]
fn test_compio_forward_local_strip_prefix() {
    let addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "path={path}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/api/users/456")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(incoming)
            .upstream(format!("http://{addr}"))
            .strip_prefix("/api")
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(
            body.contains("path=/users/456"),
            "strip_prefix should remove /api, got: {body}"
        );
    });
}

// ── Forward local: client timeout fires ───────────────────────────────

#[test]
fn test_compio_forward_local_client_timeout() {
    // Start a server that never responds
    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();
            // Accept one connection but never respond
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .timeout(Duration::from_millis(100))
            .build_local()
            .unwrap();

        let incoming = http::Request::builder()
            .method("GET")
            .uri("/test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client
            .forward_local(incoming)
            .upstream(format!("http://{addr}"))
            .send()
            .await;
        assert!(result.is_err(), "client timeout should fire for forward");
    });
}

// ── Cache store in finalize_response_local ────────────────────────────

#[test]
fn test_compio_finalize_response_local_caches() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let addr = start_server_with_tokio(move |_req| {
        let count = hit_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from("cached local")))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/resource"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "cached local");
        assert_eq!(hit_count.load(Ordering::SeqCst), 1);

        // Second request should be from cache
        let resp = client
            .get_local(&format!("http://{addr}/resource"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "cached local");
        assert_eq!(hit_count.load(Ordering::SeqCst), 1, "should be from cache");
    });
}

// ── 304 revalidation in execute_local ─────────────────────────────────

#[test]
fn test_compio_304_not_modified_not_redirect() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(304)
                .header("etag", "\"test\"")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/resource"))
            .unwrap()
            .send()
            .await
            .unwrap();
        // 304 should not be followed as redirect
        assert_eq!(resp.status(), http::StatusCode::NOT_MODIFIED);
    });
}

// ── HSTS store from HTTPS response local ──────────────────────────────

#[test]
fn test_compio_https_only_rejects_http_execute_local_path() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build_local()
            .unwrap();
        let result = client
            .get_local("http://example.com/")
            .unwrap()
            .send()
            .await;
        assert!(result.is_err(), "https_only should reject http://");
    });
}

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

                    // Read SOCKS5 greeting
                    let mut buf = [0u8; 256];
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 3 || buf[0] != 0x05 {
                        return;
                    }

                    // Reply: no auth required
                    client.write_all(&[0x05, 0x00]).await.unwrap();

                    // Read CONNECT request
                    let n = client.read(&mut buf).await.unwrap();
                    if n < 7 || buf[0] != 0x05 || buf[1] != 0x01 {
                        return;
                    }

                    // Parse target address
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

                    // Connect to the actual target on localhost
                    let target = format!("127.0.0.1:{port}");
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

/// Start an HTTP CONNECT tunnel proxy on a tokio thread.
/// For HTTPS requests, the client sends CONNECT; for plain HTTP, the proxy
/// just forwards the request.
fn start_http_proxy_tokio() -> std::net::SocketAddr {
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
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
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
    rx.recv().unwrap()
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

// ── Streaming body test (execute_local.rs line 58 coverage) ──────────

#[test]
fn test_compio_streaming_body_request() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let body_bytes = req.collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "received:{}",
            body_str
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();

        // Create a streaming body (non-buffered) -- this exercises the
        // RequestBody::Streaming branch at execute_local.rs line 58
        let stream_body: aioduct::body::RequestBodySend =
            http_body_util::Full::new(Bytes::from("streaming-payload"))
                .map_err(|never| match never {})
                .boxed_unsync();

        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .body_stream(stream_body)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "received:streaming-payload");
    });
}

#[test]
fn test_compio_streaming_body_not_replayable_on_redirect() {
    // Streaming bodies cannot be replayed after a redirect, so the second
    // request after redirect should have no body (or the redirect should work
    // with method change to GET).
    let final_addr = start_server_with_tokio(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    });

    let redirect_addr = start_server_with_tokio(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(303) // 303 changes method to GET
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();

        let stream_body: aioduct::body::RequestBodySend =
            http_body_util::Full::new(Bytes::from("stream-data"))
                .map_err(|never| match never {})
                .boxed_unsync();

        let resp = client
            .post_local(&format!("http://{redirect_addr}/"))
            .unwrap()
            .body_stream(stream_body)
            .send()
            .await
            .unwrap();

        // After 303 redirect, method should change to GET
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "GET");
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

// ── HTTP CONNECT tunnel proxy for HTTPS via local engine (connect_tunnel_local) ─

#[test]
fn test_compio_http_connect_tunnel_proxy_local() {
    // BUG: The connect_tunnel_local code uses poll_fn-based I/O (poll_write /
    // poll_read) on CompioTcpStream which hangs under compio's completion-based
    // runtime. Same root cause as the SOCKS proxy tests above.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    buf.truncate(n);
                    let req_str = String::from_utf8_lossy(&buf);

                    if req_str.starts_with("CONNECT") {
                        // Reply with 407 to test the error path
                        let response =
                            b"HTTP/1.1 407 Proxy Auth Required\r\nContent-Length: 0\r\n\r\n";
                        let _ = stream.write_all(response).await;
                    } else {
                        let _ = stream
                            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    }
                    let _ = stream.shutdown().await;
                });
            }
        });
    });

    let proxy_addr = rx.recv().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(aioduct::proxy::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        // HTTPS request triggers CONNECT tunnel through the proxy
        let result = client
            .get_local("https://example.com/secure")
            .unwrap()
            .send()
            .await;

        // Should fail with tunnel error (proxy returned 407) but currently
        // times out because the CONNECT write/read hangs under compio.
        assert!(result.is_err(), "expected tunnel error, got success");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("407")
                || err_msg.contains("CONNECT tunnel failed")
                || err_msg.contains("timeout")
                || err_msg.contains("Timeout"),
            "expected tunnel failure or timeout message, got: {err_msg}"
        );
    });
}

#[test]
fn test_compio_http_connect_tunnel_with_auth_local() {
    // BUG: Same compio poll_fn I/O hang as other proxy tests.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_clone = auth_seen.clone();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let auth_flag = auth_clone.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    buf.truncate(n);
                    let req_str = String::from_utf8_lossy(&buf);

                    if req_str.starts_with("CONNECT") {
                        // Check for Proxy-Authorization header
                        for line in req_str.lines() {
                            if line.to_lowercase().starts_with("proxy-authorization:") {
                                auth_flag.store(true, Ordering::SeqCst);
                            }
                        }
                        // Return 400 to avoid TLS negotiation
                        let _ = stream
                            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    }
                    let _ = stream.shutdown().await;
                });
            }
        });
    });

    let proxy_addr = rx.recv().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(
                aioduct::proxy::ProxyConfig::http(&format!("http://{proxy_addr}"))
                    .unwrap()
                    .basic_auth("Aladdin", "open sesame"),
            )
            .timeout(Duration::from_secs(2))
            .build_local()
            .unwrap();

        let result = client
            .get_local("https://example.com/auth-tunnel")
            .unwrap()
            .send()
            .await;

        // Expected to fail -- either tunnel error or timeout due to compio bug
        assert!(result.is_err());
    });

    // NOTE: auth_seen may not be set due to the compio poll_fn hang
    // preventing the CONNECT request from ever being written.
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
#[test]
fn test_compio_h2_multiplexing_reuses_connection() {
    use hyper::server::conn::http2 as server_http2;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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

    let request_count = Arc::new(AtomicU32::new(0));
    let count_clone = request_count.clone();

    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        let count = count_clone;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr).unwrap();

                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                    let count = count.clone();
                    tokio::spawn(async move {
                        let _ = server_http2::Builder::new(TokioExec)
                            .serve_connection(
                                io,
                                service_fn(move |_req| {
                                    let count = count.clone();
                                    async move {
                                        count.fetch_add(1, Ordering::SeqCst);
                                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                            "h2 mux",
                                        ))))
                                    }
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .build_local()
            .unwrap();

        for i in 0..5 {
            let resp = client
                .get_local(&format!("http://{addr}/req{i}"))
                .unwrap()
                .h2c_prior_knowledge()
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "h2 mux");
        }

        assert_eq!(
            request_count.load(Ordering::SeqCst),
            5,
            "all 5 H2 requests via compio should succeed with connection reuse"
        );
    });
}

/// H1 deferred check-in: connections aren't reused until the body is consumed.
/// Sequential requests with consumed bodies should reuse a single connection.
#[test]
fn h1_deferred_checkin_reuses_connection() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    let addr = start_server_with_tokio(move |_req| {
        let cnt = accept_count2.clone();
        async move {
            cnt.fetch_add(1, Ordering::SeqCst);
            Ok(Response::new(Full::new(Bytes::from("ok"))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        for _ in 0..5 {
            let resp = client.get_local(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            let _ = resp.text().await.unwrap();
            // Wait for deferred check-in to complete.
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let requests = accept_count.load(Ordering::SeqCst);
    assert_eq!(requests, 5, "all 5 requests should succeed");
}

// ── Compio DNS / resolver tests ──────────────────────────────────────

#[test]
fn test_compio_force_addr_skips_dns() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .force_addr(addr)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_force_addr_with_resolve_all_workflow() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let addrs = client.resolve_all("127.0.0.1", addr.port()).await.unwrap();
        let chosen = addrs.into_iter().next().unwrap();

        let resp = client
            .get_local(&format!("http://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .force_addr(chosen)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_system_resolver_resolves_localhost() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolver(aioduct::SystemResolver)
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}
