#![cfg(feature = "rustls")]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::{
    HttpEngineSend, MessageSignatureComponent, MessageSignatureConfig, MessageSignatureError,
};
use bytes::Bytes;
use http::header::HOST;
use http_body_util::Full;
use hyper::{Request, Response};

const ORIGINAL_AUTHORITY: &str = "original.example:8443";

fn authority_signature_config() -> MessageSignatureConfig {
    MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::authority())
        .component(MessageSignatureComponent::target_uri())
}

fn recording_signer(
    bases: Arc<Mutex<Vec<String>>>,
) -> impl Fn(&[u8]) -> Result<Vec<u8>, MessageSignatureError> + Send + Sync + 'static {
    move |base| {
        bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"signed".to_vec())
    }
}

fn assert_preserved_signature(bases: &Arc<Mutex<Vec<String>>>, scheme: &str) {
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(&format!(r#""@authority": {ORIGINAL_AUTHORITY}"#)),
        "{}",
        bases[0]
    );
    assert!(
        bases[0].contains(&format!(
            r#""@target-uri": {scheme}://{ORIGINAL_AUTHORITY}/resource"#
        )),
        "{}",
        bases[0]
    );
}

#[tokio::test]
async fn forward_preserve_host_sets_http2_authority() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) = aioduct_test_server::tls::tls_h2_server_with(|request| async move {
        let authority = request
            .uri()
            .authority()
            .map(http::uri::Authority::as_str)
            .unwrap_or("missing");
        let host = request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "authority={authority};host={host}"
        )))))
    })
    .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/resource")
        .header(HOST, ORIGINAL_AUTHORITY)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .on_request(|parts| parts.version = http::Version::HTTP_2)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("authority={ORIGINAL_AUTHORITY};host={ORIGINAL_AUTHORITY}")
    );
}

#[tokio::test]
async fn forward_preserve_host_uses_inbound_http2_authority_without_host() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) = aioduct_test_server::tls::tls_h2_server_with(|request| async move {
        let authority = request.uri().authority().unwrap().as_str();
        let host = request.headers().get(HOST).unwrap().to_str().unwrap();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "authority={authority};host={host}"
        )))))
    })
    .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri(format!("https://{ORIGINAL_AUTHORITY}/resource"))
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .on_request(|parts| parts.version = http::Version::HTTP_2)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("authority={ORIGINAL_AUTHORITY};host={ORIGINAL_AUTHORITY}")
    );
}

#[tokio::test]
async fn forward_http2_normalizes_equivalent_default_port_host() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        let authority = request.uri().authority().unwrap().as_str();
        let host = request.headers().get(HOST).unwrap().to_str().unwrap();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "authority={authority};host={host}"
        )))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("http://original.example/resource")
        .version(http::Version::HTTP_2)
        .header(HOST, "ORIGINAL.EXAMPLE:80")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .preserve_host()
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        "authority=original.example;host=original.example"
    );
}

#[tokio::test]
async fn forward_h1_absolute_form_ignores_mismatched_host() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        let host = request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing")
            .to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri(format!("https://{ORIGINAL_AUTHORITY}/resource"))
        .header(HOST, "different.example:9443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .preserve_host()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), ORIGINAL_AUTHORITY);
}

#[tokio::test]
async fn forward_preserve_host_sets_negotiated_http2_authority() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) = aioduct_test_server::tls::tls_h2_server_with(|request| async move {
        let authority = request
            .uri()
            .authority()
            .map(http::uri::Authority::as_str)
            .unwrap_or("missing");
        let host = request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "authority={authority};host={host}"
        )))))
    })
    .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/resource")
        .header(HOST, ORIGINAL_AUTHORITY)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("authority={ORIGINAL_AUTHORITY};host={ORIGINAL_AUTHORITY}")
    );
}

#[tokio::test]
async fn forward_hook_host_sets_negotiated_http2_authority() {
    const HOOK_AUTHORITY: &str = "hook.example:9443";
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) = aioduct_test_server::tls::tls_h2_server_with(|request| async move {
        let authority = request.uri().authority().unwrap().as_str();
        let host = request.headers().get(HOST).unwrap().to_str().unwrap();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "authority={authority};host={host}"
        )))))
    })
    .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> =
        HttpEngineSend::builder().tls(connector).build().unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/resource")
        .header(HOST, "incoming.example")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| {
            parts
                .headers
                .insert(HOST, http::HeaderValue::from_static(HOOK_AUTHORITY));
        })
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("authority={HOOK_AUTHORITY};host={HOOK_AUTHORITY}")
    );
}

#[cfg(feature = "http3")]
#[tokio::test]
async fn forward_preserve_host_sets_http3_authority() {
    let (addr, _, _) = aioduct_test_server::h3::h3_server_with(|request, _body| {
        let authority = request
            .uri()
            .authority()
            .map(http::uri::Authority::as_str)
            .unwrap_or("missing");
        let host = request
            .headers()
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        (
            http::StatusCode::OK,
            Bytes::from(format!("authority={authority};host={host}")),
        )
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/resource")
        .header(HOST, ORIGINAL_AUTHORITY)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("authority={ORIGINAL_AUTHORITY};host={ORIGINAL_AUTHORITY}")
    );
}

#[cfg(feature = "http3")]
#[tokio::test]
async fn forward_preserve_host_uses_inbound_http3_authority_without_host() {
    let (addr, _, _) = aioduct_test_server::h3::h3_server_with(|request, _body| {
        let authority = request.uri().authority().unwrap().as_str();
        let host = request.headers().get(HOST).unwrap().to_str().unwrap();
        (
            http::StatusCode::OK,
            Bytes::from(format!("authority={authority};host={host}")),
        )
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri(format!("https://{ORIGINAL_AUTHORITY}/resource"))
        .version(http::Version::HTTP_3)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("authority={ORIGINAL_AUTHORITY};host={ORIGINAL_AUTHORITY}")
    );
}

#[cfg(feature = "http3")]
#[tokio::test]
async fn forward_hook_host_sets_http3_authority() {
    const HOOK_AUTHORITY: &str = "hook-h3.example:9443";
    let (addr, _, _) = aioduct_test_server::h3::h3_server_with(|request, _body| {
        let authority = request.uri().authority().unwrap().as_str();
        let host = request.headers().get(HOST).unwrap().to_str().unwrap();
        (
            http::StatusCode::OK,
            Bytes::from(format!("authority={authority};host={host}")),
        )
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/resource")
        .header(HOST, "incoming.example")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| {
            parts.version = http::Version::HTTP_3;
            parts
                .headers
                .insert(HOST, http::HeaderValue::from_static(HOOK_AUTHORITY));
        })
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!("authority={HOOK_AUTHORITY};host={HOOK_AUTHORITY}")
    );
}

#[tokio::test]
async fn forward_preserve_host_signature_covers_http1_wire_authority() {
    let (addr, _) = aioduct_test_server::h1::h1_server().await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .message_signature(
            authority_signature_config(),
            recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .uri("/resource")
        .header(HOST, ORIGINAL_AUTHORITY)
        .body(Full::new(Bytes::new()))
        .unwrap();

    client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .preserve_host()
        .send()
        .await
        .unwrap();

    assert_preserved_signature(&bases, "http");
}

#[tokio::test]
async fn forward_preserve_host_signature_covers_negotiated_http2_wire_authority() {
    let (addr, cert, _) = aioduct_test_server::tls::tls_h2_server().await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .message_signature(
            authority_signature_config(),
            recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .uri("/resource")
        .header(HOST, ORIGINAL_AUTHORITY)
        .body(Full::new(Bytes::new()))
        .unwrap();

    client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .send()
        .await
        .unwrap();

    assert_preserved_signature(&bases, "https");
}

#[tokio::test]
async fn forward_preserve_host_signature_covers_exact_http2_wire_authority() {
    let (addr, cert, _) = aioduct_test_server::tls::tls_h2_server().await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .message_signature(
            authority_signature_config(),
            recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .uri("/resource")
        .header(HOST, ORIGINAL_AUTHORITY)
        .body(Full::new(Bytes::new()))
        .unwrap();

    client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .on_request(|parts| parts.version = http::Version::HTTP_2)
        .send()
        .await
        .unwrap();

    assert_preserved_signature(&bases, "https");
}

#[cfg(feature = "http3")]
#[tokio::test]
async fn forward_preserve_host_signature_covers_http3_wire_authority() {
    let (addr, _, _) = aioduct_test_server::h3::h3_server().await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .message_signature(
            authority_signature_config(),
            recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .uri("/resource")
        .header(HOST, ORIGINAL_AUTHORITY)
        .body(Full::new(Bytes::new()))
        .unwrap();

    client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap();

    assert_preserved_signature(&bases, "https");
}
