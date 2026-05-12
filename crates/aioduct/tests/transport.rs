#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_custom_resolver() {
    use std::pin::Pin;

    let target_addr = start_server().await;

    let resolver_addr = target_addr;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .resolver(
            move |_host: &str,
                  _port: u16|
                  -> Pin<
                Box<dyn std::future::Future<Output = std::io::Result<std::net::SocketAddr>> + Send>,
            > { Box::pin(async move { Ok(resolver_addr) }) },
        )
        .build();

    // Request to a fake host, but resolver redirects to our test server
    let resp = client
        .get("http://fake-host.invalid/")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[tokio::test]
async fn test_tcp_keepalive() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tcp_keepalive(Duration::from_secs(60))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[tokio::test]
async fn test_local_address_binding() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[tokio::test]
async fn test_http2_config_accepted() {
    let addr = start_server().await;
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2(
            aioduct::Http2Config::new()
                .initial_stream_window_size(1024 * 1024)
                .initial_connection_window_size(2 * 1024 * 1024)
                .max_frame_size(32_768)
                .adaptive_window(true)
                .keep_alive_interval(Duration::from_secs(30))
                .keep_alive_timeout(Duration::from_secs(10))
                .keep_alive_while_idle(true)
                .max_header_list_size(8192)
                .max_send_buf_size(1024 * 1024)
                .max_concurrent_reset_streams(100),
        )
        .build();

    // HTTP/1 request still works with h2 config set (config only applies to h2 connections)
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[tokio::test]
async fn test_tcp_fast_open_works() {
    let addr = start_server().await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tcp_fast_open(true)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
#[tokio::test]
async fn test_h2_prior_knowledge() {
    let addr = start_h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 response"))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "h2 response");
}
#[tokio::test]
async fn test_h2_prior_knowledge_multiple_requests() {
    let count = Arc::new(AtomicU32::new(0));
    let count_clone = count.clone();

    let addr = start_h2_server_with(move |_req| {
        let count = count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .build();

    for _ in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await;
    }
    assert_eq!(count.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn test_happy_eyeballs_single_addr() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(io, service_fn(hello))
                    .await;
            });
        }
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .resolver(move |_host: &str, _port: u16| {
            let addr = addr;
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                >
        })
        .build();

    let resp = client
        .get(&format!("http://custom-host:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
}
#[tokio::test]
async fn test_tcp_keepalive_with_interval_and_retries() {
    let addr = start_server().await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
}
#[cfg(unix)]
#[tokio::test]
async fn test_unix_socket_connection() {
    use tokio::net::UnixListener;

    let dir = std::env::temp_dir().join("aioduct_unix_test");
    let _ = std::fs::create_dir_all(&dir);
    let sock_path = dir.join("test.sock");
    let _ = std::fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path).unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(io, service_fn(hello))
                    .await;
            });
        }
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .unix_socket(&sock_path)
        .build();

    let resp = client
        .get("http://localhost/")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_dir(&dir);
}
#[tokio::test]
async fn test_happy_eyeballs_multi_addrs_integration() {
    let addr = start_server_with(move |_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("he-ok"))))
    })
    .await;

    let good_addr = addr;
    let bad_addr: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();

    struct MultiResolver {
        addrs: Vec<std::net::SocketAddr>,
    }

    impl aioduct::Resolve for MultiResolver {
        fn resolve(
            &self,
            _host: &str,
            _port: u16,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<std::net::SocketAddr>> + Send>,
        > {
            let addr = self.addrs[0];
            Box::pin(async move { Ok(addr) })
        }

        fn resolve_all(
            &self,
            _host: &str,
            _port: u16,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = std::io::Result<Vec<std::net::SocketAddr>>> + Send,
            >,
        > {
            let addrs = self.addrs.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .resolver(MultiResolver {
            addrs: vec![bad_addr, good_addr],
        })
        .build();

    let resp = client
        .get(&format!("http://multi.example.com:{}/", good_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "he-ok");
}
#[tokio::test]
#[allow(deprecated)]
async fn test_timings_http_direct() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                server_http1::Builder::new()
                    .serve_connection(io, service_fn(hello))
                    .await
                    .ok();
            });
        }
    });

    let client: HttpEngine<TokioRuntime, TcpConnector> = HttpEngine::builder(TcpConnector).build();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let timings = resp.timings().expect("timings should be present");
    assert!(timings.dns().is_some(), "DNS duration should be present");
    assert!(
        timings.tcp_connect().is_some(),
        "TCP connect duration should be present"
    );
    assert!(
        timings.tls_handshake().is_none(),
        "TLS should be None for HTTP"
    );
    assert!(
        timings.transfer().is_some(),
        "transfer duration should be present"
    );
    assert!(!timings.total().is_zero(), "total should be non-zero");
}
#[cfg(feature = "rustls")]
#[tokio::test]
#[allow(deprecated)]
async fn test_timings_https_with_tls() {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let server_config = {
        install_rustls_crypto_provider();
        let mut cfg = rustls::ServerConfig::builder_with_provider(rustls_crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der)
            .unwrap();
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Arc::new(cfg)
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(server_config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let acc = acceptor.clone();
            tokio::spawn(async move {
                let tls_stream = match acc.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);

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

                let builder = hyper::server::conn::http2::Builder::new(TokioExec);
                builder.serve_connection(io, service_fn(hello)).await.ok();
            });
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
    let client: HttpEngine<TokioRuntime, TcpConnector> = HttpEngine::builder(TcpConnector)
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    let timings = resp.timings().expect("timings should be present");
    assert!(timings.dns().is_some(), "DNS duration should be present");
    assert!(
        timings.tcp_connect().is_some(),
        "TCP connect duration should be present"
    );
    assert!(
        timings.tls_handshake().is_some(),
        "TLS handshake duration should be present for HTTPS"
    );
    assert!(
        timings.transfer().is_some(),
        "transfer duration should be present"
    );
    assert!(!timings.total().is_zero(), "total should be non-zero");
    assert!(
        timings.total() >= timings.dns().unwrap() + timings.tcp_connect().unwrap(),
        "total should be >= dns + tcp"
    );
}
