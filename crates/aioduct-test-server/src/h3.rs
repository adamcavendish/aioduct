use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use rustls::pki_types::CertificateDer;

use crate::ConnectionCounter;
use crate::tls::{crypto_provider, generate_self_signed, install_crypto_provider};

pub type H3Handler =
    dyn Fn(http::Request<()>, Bytes) -> (http::StatusCode, Bytes) + Send + Sync + 'static;

pub async fn h3_server() -> (SocketAddr, CertificateDer<'static>, ConnectionCounter) {
    h3_server_with(|_req, _body| (http::StatusCode::OK, Bytes::new())).await
}

pub async fn h3_server_with(
    handler: impl Fn(http::Request<()>, Bytes) -> (http::StatusCode, Bytes)
    + Send
    + Sync
    + Clone
    + 'static,
) -> (SocketAddr, CertificateDer<'static>, ConnectionCounter) {
    install_crypto_provider();

    let cert = generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der], cert.key_der)
        .unwrap();
    server_tls_config.alpn_protocols = vec![b"h3".to_vec()];
    server_tls_config.max_early_data_size = 0;

    let server_config = h3_quinn::quinn::ServerConfig::with_crypto(Arc::new(
        h3_quinn::quinn::crypto::rustls::QuicServerConfig::try_from(server_tls_config).unwrap(),
    ));

    let endpoint =
        h3_quinn::quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();

    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let handler = handler.clone();
            counter2.inc_connections();
            let counter3 = counter2.clone();
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
                    counter3.inc_requests();
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

    (addr, cert_der, counter)
}

pub async fn h3_server_stop_sending(
    handler: impl Fn(http::Request<()>) -> (http::StatusCode, Bytes) + Send + Sync + Clone + 'static,
) -> (SocketAddr, CertificateDer<'static>, ConnectionCounter) {
    install_crypto_provider();

    let cert = generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();

    let mut server_tls_config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der], cert.key_der)
        .unwrap();
    server_tls_config.alpn_protocols = vec![b"h3".to_vec()];
    server_tls_config.max_early_data_size = 0;

    let server_config = h3_quinn::quinn::ServerConfig::with_crypto(Arc::new(
        h3_quinn::quinn::crypto::rustls::QuicServerConfig::try_from(server_tls_config).unwrap(),
    ));

    let endpoint =
        h3_quinn::quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();

    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let handler = handler.clone();
            counter2.inc_connections();
            let counter3 = counter2.clone();
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
                    counter3.inc_requests();
                    tokio::spawn(async move {
                        let (req, mut stream) = match resolver.resolve_request().await {
                            Ok(r) => r,
                            Err(_) => return,
                        };

                        stream.stop_sending(h3::error::Code::H3_NO_ERROR);

                        let (status, resp_body) = handler(req);

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

    (addr, cert_der, counter)
}
