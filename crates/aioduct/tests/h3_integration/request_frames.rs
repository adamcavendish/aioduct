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

#[tokio::test(flavor = "current_thread")]
async fn h3_request_trailer_after_response_handoff_resets_upload_and_reuses_connection() {
    let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
    let cancelled_tx = Arc::new(Mutex::new(Some(cancelled_tx)));
    let server_cancelled = cancelled_tx.clone();
    let (addr, _, counter) =
        aioduct_test_server::h3::h3_server_streaming(move |request, mut stream| {
            let cancelled_tx = server_cancelled.clone();
            async move {
                if request.uri().path() == "/after-handoff" {
                    stream
                        .send_response(http::Response::builder().status(200).body(()).unwrap())
                        .await
                        .unwrap();
                    stream.finish().await.unwrap();

                    let cancelled = loop {
                        match stream.recv_data().await {
                            Ok(Some(_)) => {}
                            Ok(None) => break false,
                            Err(h3::error::StreamError::RemoteTerminate { code, .. }) => {
                                break code == h3::error::Code::H3_REQUEST_CANCELLED;
                            }
                            Err(_) => break false,
                        }
                    };
                    if let Some(sender) = cancelled_tx.lock().unwrap().take() {
                        let _ = sender.send(cancelled);
                    }
                } else {
                    while matches!(stream.recv_data().await, Ok(Some(_))) {}
                    stream
                        .send_response(http::Response::builder().status(204).body(()).unwrap())
                        .await
                        .unwrap();
                    stream.finish().await.unwrap();
                }
            }
        })
        .await;
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut trailers = HeaderMap::new();
    trailers.insert("x-upload-checksum", "complete".parse().unwrap());
    let frames = futures_util::stream::once(async move {
        release_rx.await.unwrap();
        Ok::<_, aioduct::Error>(hyper::body::Frame::trailers(trailers))
    });
    let body = http_body_util::StreamBody::new(frames).boxed_unsync();
    let client = h3_client();

    let response = client
        .post(&format!("https://127.0.0.1:{}/after-handoff", addr.port()))
        .unwrap()
        .body_stream(body)
        .send()
        .await
        .expect("the response should be handed off before the trailer is produced");
    release_tx.send(()).unwrap();

    assert!(
        tokio::time::timeout(Duration::from_secs(1), cancelled_rx)
            .await
            .expect("server did not observe detached upload cancellation")
            .unwrap(),
        "detached trailer rejection did not cancel the request send direction"
    );
    assert!(response.bytes().await.unwrap().is_empty());

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
