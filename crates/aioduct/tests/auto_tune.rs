#![cfg(feature = "tokio")]

use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::{AutoTuneConfig, HttpEngineSend};

#[cfg(feature = "rustls")]
fn set_http2_version(request: &mut http::Request<aioduct::body::RequestBodySend>, _: &http::Uri) {
    *request.version_mut() = http::Version::HTTP_2;
}

#[tokio::test]
async fn large_known_body_uses_h1_pool_key() {
    let (addr, _) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .auto_tune(AutoTuneConfig::default().large_body_threshold(4))
        .build()
        .expect("client should build");

    let response = client
        .post(&format!("http://{addr}/upload"))
        .expect("request URL should parse")
        .body("1234")
        .send()
        .await
        .expect("request should succeed");
    assert_eq!(response.version(), http::Version::HTTP_11);
    let _ = response.bytes().await.expect("response body should drain");

    let stats = client.pool_stats();
    assert!(
        stats
            .hosts
            .iter()
            .any(|host| host.authority == addr.to_string() && host.protocol_hint == "Http1"),
        "expected an Http1 pool key, got {:?}",
        stats.hosts
    );
}

#[tokio::test]
async fn auto_tune_disabled_keeps_auto_pool_key() {
    let (addr, _) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let response = client
        .post(&format!("http://{addr}/upload"))
        .expect("request URL should parse")
        .body("1234")
        .send()
        .await
        .expect("request should succeed");
    let _ = response.bytes().await.expect("response body should drain");

    let stats = client.pool_stats();
    assert!(
        stats
            .hosts
            .iter()
            .any(|host| host.authority == addr.to_string() && host.protocol_hint == "Auto"),
        "expected an Auto pool key, got {:?}",
        stats.hosts
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn middleware_version_is_preserved_over_auto_tune() {
    let (addr, cert, _) =
        aioduct_test_server::tls::tls_server_with(&[b"h2"], |request| async move {
            Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                bytes::Bytes::from(format!("{:?}", request.version())),
            )))
        })
        .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&cert),
        ))
        .auto_tune(AutoTuneConfig::default().large_body_threshold(1))
        .middleware(set_http2_version)
        .build()
        .expect("client should build");

    let response = client
        .post(&format!("https://localhost:{}/upload", addr.port()))
        .expect("request URL should parse")
        .body("x")
        .send()
        .await
        .expect("request should succeed");
    assert_eq!(
        response.text().await.expect("response body should drain"),
        "HTTP/2.0"
    );
}
