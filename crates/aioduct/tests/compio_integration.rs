#![cfg(all(feature = "compio", feature = "tokio"))]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .http2_prior_knowledge()
            .build_local();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .user_agent("compio-test/1.0")
            .build_local();
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
        let engine = HttpEngineLocal::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
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
            let client =
                HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
                    .tls(connector)
                    .timeout(Duration::from_secs(5))
                    .build_local();
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
            let client =
                HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
                    .add_root_certificates(&[root_cert])
                    .timeout(Duration::from_secs(5))
                    .build_local();
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
            let client =
                HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
                    .add_root_certificates(&[root_cert])
                    .min_tls_version(TlsVersion::Tls1_3)
                    .timeout(Duration::from_secs(5))
                    .build_local();
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
            let client =
                HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
                    .add_root_certificates(&[root_cert])
                    .max_tls_version(TlsVersion::Tls1_2)
                    .timeout(Duration::from_secs(5))
                    .build_local();
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
            let client =
                HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
                    .add_root_certificates(&[root_cert])
                    .tls_sni(false)
                    .timeout(Duration::from_secs(5))
                    .build_local();
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
            let client =
                HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
                    .add_root_certificates(&[root_cert])
                    .danger_accept_invalid_hostnames(true)
                    .timeout(Duration::from_secs(5))
                    .build_local();
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
            let client =
                HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
                    .add_root_certificates(&[ca_cert])
                    .identity(identity)
                    .timeout(Duration::from_secs(5))
                    .build_local();
            let resp = client.get_local(&url(addr)).unwrap().send().await.unwrap();

            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "hello compio mtls");
        });
    }
}
