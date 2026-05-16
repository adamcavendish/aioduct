#![cfg(feature = "smol")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::HttpEngineSend;
use aioduct::runtime::smol_rt::{SmolIo, SmolRuntime};

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

async fn start_server() -> SocketAddr {
    start_server_with(|req| async { hello(req).await }).await
}

async fn start_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    smol::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = SmolIo::new(stream);
            let handler = handler.clone();
            smol::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(io, service_fn(handler))
                    .await;
            })
            .detach();
        }
    })
    .detach();

    addr
}

#[test]
fn test_smol_get_request() {
    smol::block_on(async {
        let addr = start_server().await;
        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );

        let resp = client
            .get(&format!("http://{addr}/"))
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
fn test_smol_post_request() {
    smol::block_on(async {
        let addr = start_server().await;
        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );

        let resp = client
            .post(&format!("http://{addr}/"))
            .unwrap()
            .body("request body")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_smol_connection_reuse() {
    smol::block_on(async {
        let addr = start_server().await;
        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let url = format!("http://{addr}/");

        let resp1 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        let resp2 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let body = resp2.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_smol_redirect_302() {
    smol::block_on(async {
        let final_addr = start_server().await;
        let redirect_addr = start_server_with(move |_req| {
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
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let resp = client
            .get(&format!("http://{redirect_addr}/"))
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
fn test_smol_timeout_triggers() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async {
            smol::Timer::after(Duration::from_secs(5)).await;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let result = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .timeout(Duration::from_millis(50))
            .send()
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().is_timeout(), "expected Timeout error");
    });
}

#[test]
fn test_smol_custom_header() {
    smol::block_on(async {
        let addr = start_server_with(|req| async move {
            let custom = req
                .headers()
                .get("x-custom")
                .map(|v| v.to_str().unwrap_or(""))
                .unwrap_or("missing");
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom.to_string()))))
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .header_str("x-custom", "smol-value")
            .unwrap()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert_eq!(body, "smol-value");
    });
}

async fn start_h2_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send + 'static,
{
    use hyper::server::conn::http2 as server_http2;

    #[derive(Clone)]
    struct SmolExec;
    impl<F> hyper::rt::Executor<F> for SmolExec
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            smol::spawn(fut).detach();
        }
    }

    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    smol::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = SmolIo::new(stream);
            let handler = handler.clone();
            smol::spawn(async move {
                let _ = server_http2::Builder::new(SmolExec)
                    .serve_connection(io, service_fn(handler))
                    .await;
            })
            .detach();
        }
    })
    .detach();

    addr
}

#[test]
fn test_smol_h2_prior_knowledge() {
    smol::block_on(async {
        let addr = start_h2_server_with(|_req| async {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 smol"))))
        })
        .await;

        let client =
            HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::builder(
                aioduct::runtime::smol_rt::TcpConnector,
            )
            .http2_prior_knowledge()
            .build();

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "h2 smol");
    });
}

#[test]
fn test_smol_h2_multiple_requests() {
    smol::block_on(async {
        let addr = start_h2_server_with(|_req| async {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
        })
        .await;

        let client =
            HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::builder(
                aioduct::runtime::smol_rt::TcpConnector,
            )
            .http2_prior_knowledge()
            .build();
        let url = format!("http://{addr}/");

        let resp1 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "h2 ok");

        let resp2 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "h2 ok");
    });
}

#[test]
fn test_smol_large_body() {
    smol::block_on(async {
        let addr = start_server_with(|req| async move {
            let body = req.collect().await.unwrap().to_bytes();
            let len = body.len();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let payload = "x".repeat(1024 * 1024);
        let resp = client
            .post(&format!("http://{addr}/"))
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
fn test_smol_h2_large_body() {
    smol::block_on(async {
        let addr = start_h2_server_with(|req| async move {
            let body = req.collect().await.unwrap().to_bytes();
            let len = body.len();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
        })
        .await;

        let client =
            HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::builder(
                aioduct::runtime::smol_rt::TcpConnector,
            )
            .http2_prior_knowledge()
            .build();
        let payload = "x".repeat(1024 * 1024);
        let resp = client
            .post(&format!("http://{addr}/"))
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
fn test_smol_large_response_body() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async move {
            let body = "y".repeat(512 * 1024);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let resp = client
            .get(&format!("http://{addr}/"))
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
fn test_smol_connection_pool_reuse_after_body_consumed() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("pool test"))))
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let url = format!("http://{addr}/");

        for _ in 0..5 {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "pool test");
        }
    });
}

#[test]
fn test_smol_head_request() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "1000")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let resp = client
            .request(http::Method::HEAD, &format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-length")
                .unwrap()
                .to_str()
                .unwrap(),
            "1000"
        );
        assert_eq!(resp.text().await.unwrap(), "");
    });
}

#[test]
fn test_smol_multiple_headers_same_name() {
    smol::block_on(async {
        let addr = start_server_with(|req| async move {
            let vals: Vec<&str> = req
                .headers()
                .get_all("x-multi")
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(vals.join(",")))))
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );
        let mut headers = http::HeaderMap::new();
        headers.append("x-multi", "a".parse().unwrap());
        headers.append("x-multi", "b".parse().unwrap());

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .headers(headers)
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert_eq!(body, "a,b");
    });
}

#[test]
fn test_smol_proxy_auth_for_plain_http() {
    use std::sync::atomic::{AtomicBool, Ordering};

    smol::block_on(async {
        let auth_seen = Arc::new(AtomicBool::new(false));
        let auth_seen_clone = auth_seen.clone();

        let proxy_addr = start_server_with(move |req| {
            let auth_seen = auth_seen_clone.clone();
            async move {
                if let Some(auth) = req.headers().get("proxy-authorization") {
                    let auth_str = auth.to_str().unwrap_or("");
                    if auth_str == "Basic dXNlcjpwYXNz" {
                        auth_seen.store(true, Ordering::SeqCst);
                    }
                }
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("proxied"))))
            }
        })
        .await;

        let client =
            HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::builder(
                aioduct::runtime::smol_rt::TcpConnector,
            )
            .proxy(
                aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                    .unwrap()
                    .basic_auth("user", "pass"),
            )
            .build();

        let resp = client
            .get("http://example.com/test")
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert!(
            auth_seen.load(Ordering::SeqCst),
            "smol client should inject Proxy-Authorization for plain HTTP proxy"
        );
    });
}

#[test]
fn test_smol_redirect_follows() {
    smol::block_on(async {
        let final_addr = start_server().await;

        let redirect_addr = start_server_with(move |_req| {
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
        })
        .await;

        let client = HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::new(
            aioduct::runtime::smol_rt::TcpConnector,
        );

        let resp = client
            .get(&format!("http://{redirect_addr}/start"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn test_smol_default_headers() {
    smol::block_on(async {
        let addr = start_server_with(|req| async move {
            let val = req
                .headers()
                .get("x-custom-default")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_default();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
        })
        .await;

        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::HeaderName::from_static("x-custom-default"),
            http::header::HeaderValue::from_static("smol-default"),
        );
        let client =
            HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::builder(
                aioduct::runtime::smol_rt::TcpConnector,
            )
            .default_headers(headers)
            .build();

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "smol-default");
    });
}

#[test]
fn test_smol_tcp_keepalive_request() {
    smol::block_on(async {
        let addr = start_server().await;
        let client =
            HttpEngineSend::<SmolRuntime, aioduct::runtime::smol_rt::TcpConnector>::builder(
                aioduct::runtime::smol_rt::TcpConnector,
            )
            .tcp_keepalive(Duration::from_secs(60))
            .tcp_keepalive_interval(Duration::from_secs(10))
            .tcp_keepalive_retries(3)
            .build();

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

// ── Smol TLS integration tests ─────────────────────────────────────
#[cfg(all(feature = "smol", feature = "tokio", feature = "rustls"))]
mod smol_tls_tests {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Response;
    use hyper::service::service_fn;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    use aioduct::HttpEngineSend;
    use aioduct::runtime::smol_rt::{SmolRuntime, TcpConnector};

    fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    fn install_crypto() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

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

    fn start_tls_h1_server(
        cert_der: CertificateDer<'static>,
        key_der: PrivateKeyDer<'static>,
    ) -> SocketAddr {
        let mut config = rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);

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
                                    Ok::<_, std::convert::Infallible>(Response::new(Full::new(
                                        Bytes::from("hello smol tls"),
                                    )))
                                }),
                            )
                            .await;
                    });
                }
            });
        });
        rx.recv().unwrap()
    }

    #[test]
    fn smol_tls_basic_h1_get() {
        install_crypto();
        let ca = generate_ca();
        let (cert, key) = generate_signed_cert(&ca, vec!["localhost".into()]);
        let addr = start_tls_h1_server(cert, key);

        smol::block_on(async {
            let mut root_store = rustls::RootCertStore::empty();
            root_store.add(ca.cert_der.clone()).unwrap();
            let config = rustls::ClientConfig::builder_with_provider(crypto_provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(root_store)
                .with_no_client_auth();
            let tls_config = aioduct::tls::RustlsConnector::new(Arc::new(config));

            let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder(TcpConnector)
                .tls(tls_config)
                .timeout(std::time::Duration::from_secs(5))
                .resolve("localhost", addr)
                .build();

            let resp = client
                .get(&format!("https://localhost:{}/", addr.port()))
                .unwrap()
                .send()
                .await
                .unwrap();

            assert_eq!(resp.status(), 200);
            let body = resp.text().await.unwrap();
            assert_eq!(body, "hello smol tls");
        });
    }

    #[test]
    fn smol_tls_connection_reuse() {
        install_crypto();
        let ca = generate_ca();
        let (cert, key) = generate_signed_cert(&ca, vec!["localhost".into()]);
        let addr = start_tls_h1_server(cert, key);

        smol::block_on(async {
            let mut root_store = rustls::RootCertStore::empty();
            root_store.add(ca.cert_der.clone()).unwrap();
            let config = rustls::ClientConfig::builder_with_provider(crypto_provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(root_store)
                .with_no_client_auth();
            let tls_config = aioduct::tls::RustlsConnector::new(Arc::new(config));

            let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder(TcpConnector)
                .tls(tls_config)
                .pool_idle_timeout(std::time::Duration::from_secs(30))
                .timeout(std::time::Duration::from_secs(5))
                .resolve("localhost", addr)
                .build();

            let url = format!("https://localhost:{}/", addr.port());

            let resp1 = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp1.status(), 200);
            let _ = resp1.text().await.unwrap();

            let resp2 = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp2.status(), 200);
            assert_eq!(resp2.text().await.unwrap(), "hello smol tls");
        });
    }
}
