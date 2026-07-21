use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::HeaderMap;
use http_body_util::BodyExt as _;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

fn h3_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

#[tokio::test]
async fn h3_request_trailers_fail_closed() {
    let (addr, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            while matches!(stream.recv_data().await, Ok(Some(_))) {}
            let _ = stream.recv_trailers().await;
        })
        .await;
    let mut trailers = HeaderMap::new();
    trailers.insert("x-upload-checksum", "complete".parse().unwrap());
    let frames = futures_util::stream::iter([
        Ok::<_, aioduct::Error>(hyper::body::Frame::data(Bytes::from_static(b"payload"))),
        Ok(hyper::body::Frame::trailers(trailers)),
    ]);
    let body = http_body_util::StreamBody::new(frames).boxed_unsync();

    let error = h3_client()
        .post(&format!("https://127.0.0.1:{}/trailers", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(error.error(), aioduct::Error::Unsupported(message) if message.contains("request trailers")),
        "{error:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn h3_request_trailers_fail_closed_when_an_early_response_races_them() {
    let (release_trailer, trailer_released) = tokio::sync::oneshot::channel();
    let release_trailer = Arc::new(Mutex::new(Some(release_trailer)));
    let server_release = release_trailer.clone();
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
        let release_trailer = server_release.clone();
        async move {
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            if let Some(release) = release_trailer.lock().unwrap().take() {
                let _ = release.send(());
            }
            stream.finish().await.unwrap();
        }
    })
    .await;
    let mut trailers = HeaderMap::new();
    trailers.insert("x-upload-checksum", "complete".parse().unwrap());
    let frames = futures_util::stream::once(async move {
        trailer_released.await.unwrap();
        Ok::<_, aioduct::Error>(hyper::body::Frame::trailers(trailers))
    });
    let body = http_body_util::StreamBody::new(frames).boxed_unsync();

    let error = h3_client()
        .post(&format!(
            "https://127.0.0.1:{}/early-response-trailers",
            addr.port()
        ))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(error.error(), aioduct::Error::Unsupported(message) if message.contains("request trailers")),
        "{error:?}"
    );
}

#[tokio::test]
async fn h3_response_trailers_fail_closed() {
    let (addr, _, counter) =
        aioduct_test_server::h3::h3_server_streaming(|request, mut stream| async move {
            while matches!(stream.recv_data().await, Ok(Some(_))) {}
            let _ = stream.recv_trailers().await;
            let status = if request.uri().path() == "/trailers" {
                http::StatusCode::OK
            } else {
                http::StatusCode::NO_CONTENT
            };
            stream
                .send_response(http::Response::builder().status(status).body(()).unwrap())
                .await
                .unwrap();
            if request.uri().path() == "/trailers" {
                let mut trailers = HeaderMap::new();
                trailers.insert("x-checksum", "complete".parse().unwrap());
                stream.send_trailers(trailers).await.unwrap();
            } else {
                stream.finish().await.unwrap();
            }
        })
        .await;

    let client = h3_client();
    let response = client
        .get(&format!("https://127.0.0.1:{}/trailers", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    let error = response.bytes().await.unwrap_err();

    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("response trailers")),
        "{error:?}"
    );

    let follow_up = client
        .get(&format!("https://127.0.0.1:{}/follow-up", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(follow_up.status(), http::StatusCode::NO_CONTENT);
    assert!(follow_up.bytes().await.unwrap().is_empty());
    assert_eq!(counter.connections(), 1);
    assert_eq!(counter.requests(), 2);
}
