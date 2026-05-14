#![cfg(all(feature = "tokio", feature = "http3", feature = "rustls"))]

use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::tls::{crypto_provider, install_crypto_provider};

use std::sync::Arc;

use bytes::Bytes;
use h3_quinn::quinn;

async fn build_h3_server(
    handler: impl Fn(http::Request<()>, Bytes) -> (http::StatusCode, Bytes)
    + Send
    + Sync
    + Clone
    + 'static,
) -> std::net::SocketAddr {
    install_crypto_provider();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .unwrap();
    server_tls_config.alpn_protocols = vec![b"h3".to_vec()];
    server_tls_config.max_early_data_size = 0;

    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_tls_config).unwrap(),
    ));

    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let handler = handler.clone();
            tokio::spawn(async move {
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let mut h3_conn: h3::server::Connection<h3_quinn::Connection, Bytes> =
                    match h3::server::Connection::new(h3_quinn::Connection::new(conn)).await {
                        Ok(c) => c,
                        Err(_) => return,
                    };

                loop {
                    let resolver = match h3_conn.accept().await {
                        Ok(Some(r)) => r,
                        Ok(None) => break,
                        Err(_) => break,
                    };
                    let handler = handler.clone();
                    tokio::spawn(async move {
                        let (req, mut stream) = match resolver.resolve_request().await {
                            Ok(r) => r,
                            Err(_) => return,
                        };

                        let mut body_buf = Vec::new();
                        while let Some(mut chunk) = stream.recv_data().await.unwrap_or(None) {
                            use bytes::Buf;
                            body_buf.extend_from_slice(chunk.chunk());
                            chunk.advance(chunk.remaining());
                        }
                        let req_body = Bytes::from(body_buf);

                        let (status, resp_body) = handler(req, req_body);

                        let resp = http::Response::builder().status(status).body(()).unwrap();
                        if stream.send_response(resp).await.is_err() {
                            return;
                        }
                        if !resp_body.is_empty() {
                            let _ = stream.send_data(resp_body).await;
                        }
                        let _ = stream.finish().await;
                    });
                }
            });
        }
    });

    addr
}

#[tokio::test]
async fn h3_basic_get() {
    let addr = build_h3_server(|_req, _body| (http::StatusCode::OK, Bytes::new())).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_3);
}

#[tokio::test]
async fn h3_post_with_body() {
    let addr = build_h3_server(|req, body| {
        assert_eq!(req.method(), http::Method::POST);
        assert_eq!(&body[..], b"hello");
        (http::StatusCode::OK, Bytes::new())
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build();

    let resp = client
        .post(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .body("hello")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_3);
}

#[tokio::test]
async fn h3_response_body() {
    let addr = build_h3_server(|_req, _body| (http::StatusCode::OK, Bytes::from("hello h3"))).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_3);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello h3");
}

#[tokio::test]
async fn h3_concurrent_requests() {
    let addr = build_h3_server(|req, _body| {
        let path = req.uri().path().to_string();
        (http::StatusCode::OK, Bytes::from(format!("path={path}")))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build();

    let mut handles = Vec::new();
    for i in 0..5 {
        let client = client.clone();
        let port = addr.port();
        handles.push(tokio::spawn(async move {
            let resp = client
                .get(&format!("https://localhost:{port}/{i}"))
                .unwrap()
                .send()
                .await
                .unwrap();
            assert_eq!(resp.version(), http::Version::HTTP_3);
            resp.text().await.unwrap()
        }));
    }

    for (i, handle) in handles.into_iter().enumerate() {
        let body = handle.await.unwrap();
        assert_eq!(body, format!("path=/{i}"));
    }
}

#[tokio::test]
async fn h3_connection_refused() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_millis(500))
        .build();

    let result = client.get("https://127.0.0.1:1/").unwrap().send().await;

    assert!(result.is_err());
}
