use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::ConnectionCounter;
use crate::TokioExec;
use crate::h1::HandlerResult;

pub struct TlsCert {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivateKeyDer<'static>,
}

pub fn generate_self_signed(domains: &[&str]) -> TlsCert {
    let names: Vec<String> = domains.iter().map(|s| s.to_string()).collect();
    let cert = rcgen::generate_simple_self_signed(names).unwrap();
    TlsCert {
        cert_der: CertificateDer::from(cert.cert.der().to_vec()),
        key_der: PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
    }
}

pub fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(crypto_provider_value())
}

pub fn crypto_provider_value() -> rustls::crypto::CryptoProvider {
    #[cfg(feature = "rustls")]
    {
        rustls::crypto::ring::default_provider()
    }
}

pub fn install_crypto_provider() {
    let _ = crypto_provider_value().install_default();
}

fn make_server_config(cert: &TlsCert, alpn: &[&[u8]]) -> Arc<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
        .unwrap();
    config.alpn_protocols = alpn.iter().map(|a| a.to_vec()).collect();
    Arc::new(config)
}

pub async fn tls_h1_server(
    alpn: &[&[u8]],
) -> (SocketAddr, CertificateDer<'static>, ConnectionCounter) {
    let cert = generate_self_signed(&["localhost"]);
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();
    let cert_der = cert.cert_der.clone();
    let config = make_server_config(&cert, alpn);
    let acceptor = TlsAcceptor::from(config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let acceptor = acceptor.clone();
            let counter3 = counter2.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = crate::TokioIo::new(tls_stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |_req| {
                            counter3.inc_requests();
                            async {
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                    "hello tls",
                                ))))
                            }
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, cert_der, counter)
}

pub async fn tls_h2_server() -> (SocketAddr, CertificateDer<'static>, ConnectionCounter) {
    tls_h2_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("hello tls"))))
    })
    .await
}

pub async fn tls_h2_server_with<F, Fut>(
    handler: F,
) -> (SocketAddr, CertificateDer<'static>, ConnectionCounter)
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: Future<Output = HandlerResult> + Send + 'static,
{
    let cert = generate_self_signed(&["localhost"]);
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();
    let cert_der = cert.cert_der.clone();
    let config = make_server_config(&cert, &[b"h2", b"http/1.1"]);
    let acceptor = TlsAcceptor::from(config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let acceptor = acceptor.clone();
            let handler = handler.clone();
            let counter3 = counter2.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = crate::TokioIo::new(tls_stream);
                let _ = hyper::server::conn::http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            counter3.inc_requests();
                            let handler = handler.clone();
                            async move { handler(req).await }
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, cert_der, counter)
}

pub async fn tls_server_with<F, Fut>(
    alpn: &[&[u8]],
    handler: F,
) -> (SocketAddr, CertificateDer<'static>, ConnectionCounter)
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: Future<Output = HandlerResult> + Send + 'static,
{
    let cert = generate_self_signed(&["localhost"]);
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();
    let cert_der = cert.cert_der.clone();
    let use_h2 = alpn.iter().any(|a| *a == b"h2");
    let config = make_server_config(&cert, alpn);
    let acceptor = TlsAcceptor::from(config);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let acceptor = acceptor.clone();
            let handler = handler.clone();
            let counter3 = counter2.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let io = crate::TokioIo::new(tls_stream);
                if use_h2 {
                    let _ = hyper::server::conn::http2::Builder::new(TokioExec)
                        .serve_connection(
                            io,
                            service_fn(move |req| {
                                counter3.inc_requests();
                                let handler = handler.clone();
                                async move { handler(req).await }
                            }),
                        )
                        .await;
                } else {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            io,
                            service_fn(move |req| {
                                counter3.inc_requests();
                                let handler = handler.clone();
                                async move { handler(req).await }
                            }),
                        )
                        .await;
                }
            });
        }
    });

    (addr, cert_der, counter)
}

pub fn make_client_config(cert_der: &CertificateDer<'static>) -> Arc<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(cert_der.clone()).unwrap();
    let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_root_certificates(root_store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}
