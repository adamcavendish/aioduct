use std::convert::Infallible;
#[cfg(feature = "rustls")]
use std::sync::Arc;
#[cfg(feature = "rustls")]
use std::sync::atomic::{AtomicUsize, Ordering};

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
#[cfg(feature = "rustls")]
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

async fn start_h1_version_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|req: Request<hyper::body::Incoming>| async move {
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                                "{:?}",
                                req.version()
                            )))))
                        }),
                    )
                    .await;
            });
        }
    });

    addr
}

fn empty_request(version: http::Version) -> Request<Full<Bytes>> {
    Request::builder()
        .method(http::Method::GET)
        .uri("/ingress")
        .version(version)
        .body(Full::new(Bytes::new()))
        .unwrap()
}

#[tokio::test]
async fn forward_all_ingress_versions_canonicalize_to_http11() {
    let addr = start_h1_version_server().await;
    let upstream = format!("http://{addr}").parse::<http::Uri>().unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    for ingress in [
        http::Version::HTTP_10,
        http::Version::HTTP_11,
        http::Version::HTTP_2,
        http::Version::HTTP_3,
    ] {
        let response = client
            .forward(crate::valid_forward_request(empty_request(ingress)))
            .upstream(upstream.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "HTTP/1.1");
    }
}

#[tokio::test]
async fn forward_http3_ingress_canonicalizes_to_h2c() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{:?}",
            req.version()
        )))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let response = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_3,
        )))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "HTTP/2.0");
}

#[tokio::test]
async fn forward_hook_uri_is_finalized_after_upstream_rewrite() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let _ = http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                        req.uri().to_string(),
                    ))))
                }),
            )
            .await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let response = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream(format!("http://{addr}/base").parse::<http::Uri>().unwrap())
        .on_request(|parts| {
            parts.uri = "/hooked?q=yes".parse().unwrap();
        })
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "/hooked?q=yes");
}

#[tokio::test]
async fn invalid_post_hook_protocol_metadata_fails_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let error = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_request(|parts| {
            parts
                .extensions
                .insert(aioduct::Protocol::from_static("websocket"));
        })
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::Unsupported(_)));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "invalid protocol metadata must be rejected before TCP I/O"
    );
}

#[cfg(feature = "rustls")]
async fn start_negotiated_tls_server() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicUsize>,
) {
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = connections.clone();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_connections.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls = match acceptor.accept(stream).await {
                    Ok(tls) => tls,
                    Err(_) => return,
                };
                let is_h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());
                let io = aioduct::runtime::tokio_rt::TokioIo::new(tls);
                let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "{:?}",
                        req.version()
                    )))))
                });
                if is_h2 {
                    let _ = http2::Builder::new(aioduct_test_server::TokioExec)
                        .serve_connection(io, service)
                        .await;
                } else {
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                }
            });
        }
    });

    (addr, cert_der, connections)
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_exact_tls_protocols_use_distinct_pools_and_alpn() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, connections) = start_negotiated_tls_server().await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let upstream = format!("https://localhost:{}", addr.port())
        .parse::<http::Uri>()
        .unwrap();

    let auto = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream(upstream.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(auto.text().await.unwrap(), "HTTP/2.0");

    let exact_h1 = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_2,
        )))
        .upstream(upstream.clone())
        .on_request(|parts| parts.version = http::Version::HTTP_11)
        .send()
        .await
        .unwrap();
    assert_eq!(exact_h1.text().await.unwrap(), "HTTP/1.1");

    for _ in 0..2 {
        let exact_h2 = client
            .forward(crate::valid_forward_request(empty_request(
                http::Version::HTTP_11,
            )))
            .upstream(upstream.clone())
            .on_request(|parts| parts.version = http::Version::HTTP_2)
            .send()
            .await
            .unwrap();
        assert_eq!(exact_h2.text().await.unwrap(), "HTTP/2.0");
    }

    assert_eq!(connections.load(Ordering::SeqCst), 3);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_negotiated_https_h2_uses_https_scheme() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) = aioduct_test_server::tls::tls_h2_server_with(|req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "scheme={},authority={}",
            req.uri().scheme_str().unwrap_or("missing"),
            req.uri()
                .authority()
                .map(http::uri::Authority::as_str)
                .unwrap_or("missing")
        )))))
    })
    .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();

    let response = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("scheme=https,authority=localhost:{}", addr.port())
    );
}

#[tokio::test]
async fn forward_hook_authority_rewrites_host_to_final_authority() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let host = req
            .headers()
            .get(http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host.to_owned()))))
    })
    .await;
    let final_uri = format!("http://127.0.0.1:{}/hooked", addr.port())
        .parse::<http::Uri>()
        .unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let response = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream("http://original.invalid".parse::<http::Uri>().unwrap())
        .on_request(move |parts| parts.uri = final_uri.clone())
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("127.0.0.1:{}", addr.port())
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_h2c_to_https_fails_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .build()
        .unwrap();
    let error = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::Unsupported(_)));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "h2c over TLS must be rejected before TCP I/O"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_exact_http2_rejects_http11_only_tls_upstream() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) = aioduct_test_server::tls::tls_h1_server(&[]).await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();

    let error = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_2)
        .send()
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("upstream did not negotiate required HTTP/2 ALPN"),
        "{error:?}"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_hook_can_select_exact_http3() {
    let (addr, _, _) = aioduct_test_server::h3::h3_server_with(|req, _body| {
        (
            http::StatusCode::OK,
            Bytes::from(format!("{:?}", req.version())),
        )
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(empty_request(
            http::Version::HTTP_11,
        )))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "HTTP/3.0");
}
