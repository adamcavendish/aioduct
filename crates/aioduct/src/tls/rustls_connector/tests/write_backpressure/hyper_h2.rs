use super::super::*;
use super::multipart::multipart_body;
use super::support::connected_budget_client_with_h2_server;

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};

use crate::runtime::TokioRuntime;

fn has_io_error_kind(mut error: &(dyn std::error::Error + 'static), kind: io::ErrorKind) -> bool {
    loop {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == kind)
        {
            return true;
        }
        let Some(source) = error.source() else {
            return false;
        };
        error = source;
    }
}

#[tokio::test]
async fn hyper_h2_multipart_upload_resumes_after_tls_backpressure() {
    let (mut client_tls, server_tls, control) = connected_budget_client_with_h2_server().await;
    client_tls.tls.set_buffer_limit(Some(1024));

    let boundary = "AioductH2BackpressureBoundary";
    let file: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let multipart = multipart_body(boundary, &file);
    let expected_multipart = multipart.clone();

    let server_task = tokio::spawn(async move {
        hyper::server::conn::http2::Builder::new(crate::runtime::executor::poll_executor::<
            TokioRuntime,
        >())
        .serve_connection(
            server_tls,
            hyper::service::service_fn(move |req: http::Request<hyper::body::Incoming>| {
                let expected_multipart = expected_multipart.clone();
                async move {
                    assert_eq!(req.method(), http::Method::POST);
                    assert_eq!(req.uri().path(), "/api/v2/ocr/jobs");
                    assert_eq!(
                        req.headers().get(http::header::CONTENT_TYPE).unwrap(),
                        &format!("multipart/form-data; boundary={boundary}")
                    );
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    assert_eq!(body, expected_multipart);
                    Ok::<_, Infallible>(hyper::Response::new(Full::new(Bytes::from_static(
                        b"upload ok",
                    ))))
                }
            }),
        )
        .await
    });

    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        crate::runtime::executor::poll_executor::<TokioRuntime>(),
        client_tls,
    )
    .await
    .unwrap();
    let connection_task = tokio::spawn(connection);
    sender.ready().await.unwrap();

    control.set_write_budget(Some(1));
    let release_control = control.clone();
    let release_task = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(2),
            release_control.wait_for_blocked_writes(2),
        )
        .await
        .expect("HTTP/2 TLS transport should become backpressured twice");
        release_control.set_write_budget(None);
    });

    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri("https://localhost/api/v2/ocr/jobs")
        .header(
            http::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(http::header::CONTENT_LENGTH, multipart.len())
        .body(Full::new(Bytes::from(multipart)))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), sender.send_request(request))
        .await
        .expect("HTTP/2 request should not hang")
        .expect("HTTP/2 request should survive TLS backpressure");
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"upload ok")
    );

    drop(sender);
    release_task.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), connection_task)
        .await
        .expect("HTTP/2 client connection should finish")
        .expect("HTTP/2 client connection task should not panic")
        .expect("HTTP/2 client connection should close cleanly");
    let server_result = tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("HTTP/2 server connection should finish")
        .expect("HTTP/2 server connection task should not panic");
    if let Err(error) = server_result {
        assert!(
            has_io_error_kind(&error, io::ErrorKind::BrokenPipe),
            "HTTP/2 server must not fail from trailing or corrupt plaintext: {error}"
        );
    }
}
