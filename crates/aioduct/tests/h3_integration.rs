#![cfg(all(feature = "tokio", feature = "http3", feature = "rustls"))]

use std::time::Duration;

use bytes::Bytes;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::tls::install_crypto_provider;

#[path = "h3_integration/early_response.rs"]
mod early_response;
#[path = "h3_integration/request_frames.rs"]
mod request_frames;
#[path = "h3_integration/request_streaming.rs"]
mod request_streaming;
#[path = "h3_integration/retry_evidence.rs"]
mod retry_evidence;
#[path = "h3_integration/transport_progress.rs"]
mod transport_progress;

#[tokio::test]
async fn h3_basic_get() {
    let (addr, _cert, _counter) = aioduct_test_server::h3::h3_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_3);
}

#[tokio::test]
async fn h3_post_with_body() {
    let (addr, _cert, _counter) = aioduct_test_server::h3::h3_server_with(|req, body| {
        assert_eq!(req.method(), http::Method::POST);
        assert_eq!(&body[..], b"hello");
        (http::StatusCode::OK, Bytes::new())
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();

    let resp = client
        .post(&format!("https://127.0.0.1:{}/", addr.port()))
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
    let (addr, _cert, _counter) = aioduct_test_server::h3::h3_server_with(|_req, _body| {
        (http::StatusCode::OK, Bytes::from("hello h3"))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://127.0.0.1:{}/", addr.port()))
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
    let (addr, _cert, _counter) = aioduct_test_server::h3::h3_server_with(|req, _body| {
        let path = req.uri().path().to_string();
        (http::StatusCode::OK, Bytes::from(format!("path={path}")))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..5 {
        let client = client.clone();
        let port = addr.port();
        handles.push(tokio::spawn(async move {
            let resp = client
                .get(&format!("https://127.0.0.1:{port}/{i}"))
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
    install_crypto_provider();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();

    let result = client.get("https://127.0.0.1:1/").unwrap().send().await;

    assert!(result.is_err());
}

#[tokio::test]
async fn h3_stop_sending_no_error() {
    let (addr, _cert, _counter) = aioduct_test_server::h3::h3_server_stop_sending(|_req| {
        (http::StatusCode::OK, Bytes::from("accepted"))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();

    let resp = client
        .post(&format!("https://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .body("request body that will be stopped")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.version(), http::Version::HTTP_3);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "accepted");
}
