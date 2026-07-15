#![cfg(feature = "rustls")]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::{MessageSignatureComponent, MessageSignatureConfig, MessageSignatureError};
use bytes::Bytes;
use http::header::TE;
use http_body_util::Full;
use hyper::{Request, Response};

async fn assert_negotiated_te(alpn: &[&[u8]], expected_version: &str, expected_te: &str) {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) = aioduct_test_server::tls::tls_server_with(alpn, |request| async move {
        let te = request
            .headers()
            .get(TE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{:?};{te}",
            request.version()
        )))))
    })
    .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/trailers")
        .header(TE, "trailers")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
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
        format!("{expected_version};{expected_te}")
    );
}

#[tokio::test]
async fn forward_auto_te_trailers_is_removed_after_tls_negotiates_http11() {
    assert_negotiated_te(&[b"http/1.1"], "HTTP/1.1", "missing").await;
}

#[tokio::test]
async fn forward_auto_te_trailers_is_canonicalized_after_tls_negotiates_http2() {
    assert_negotiated_te(&[b"h2", b"http/1.1"], "HTTP/2.0", "trailers").await;
}

#[tokio::test]
async fn forward_auto_invalid_te_is_accepted_when_tls_negotiates_http11() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |request| async move {
            assert!(!request.headers().contains_key(TE));
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
        })
        .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .uri("/resource")
        .header(TE, "gzip")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn forward_auto_invalid_te_is_rejected_after_tls_negotiates_http2() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, counter) =
        aioduct_test_server::tls::tls_server_with(&[b"h2", b"http/1.1"], |_request| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"unexpected"))))
        })
        .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .uri("/resource")
        .header(TE, "gzip")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)));
    assert_eq!(counter.requests(), 0);
}

#[tokio::test]
async fn forward_auto_rejects_invalid_header_value_after_tls_negotiates_http2() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, counter) =
        aioduct_test_server::tls::tls_server_with(&[b"h2", b"http/1.1"], |_request| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"unexpected"))))
        })
        .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .uri("/resource")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| {
            parts.headers.insert(
                "x-invalid",
                http::HeaderValue::from_bytes(b" leading-space").unwrap(),
            );
        })
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    assert_eq!(counter.requests(), 0);
}

#[tokio::test]
async fn forward_signature_covers_te_after_tls_negotiates_http2() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) =
        aioduct_test_server::tls::tls_server_with(&[b"h2", b"http/1.1"], |request| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                request
                    .headers()
                    .get("signature")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("missing")
                    .to_owned(),
            ))))
        })
        .await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"signed".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(TE));
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .message_signature(config, signer)
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/trailers")
        .header(TE, "trailers")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "sig1=:c2lnbmVk:");
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].contains(r#""te": trailers"#), "{}", bases[0]);
}
