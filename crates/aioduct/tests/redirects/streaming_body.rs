use std::convert::Infallible;
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h1::h1_server_with;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Response;

// Streaming-body redirect edge tests.
// Tests below verify subtle edge cases in redirect handling.

// 307 redirects with streaming bodies should preserve the body or return an error.
// Non-replayable streaming bodies must not be followed as POST with an empty body.
#[tokio::test]
async fn redirect_307_streaming_body_should_not_silently_lose_body() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(307)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} body_len={} body={body_str}",
                body_bytes.len()
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("streaming-payload"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let result = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let body = resp.text().await.unwrap();
            assert!(
                body.contains("streaming-payload"),
                "BUG: 307 redirect with streaming body silently drops the body. \
                 Expected body to contain 'streaming-payload', got: {body}"
            );
        }
        Err(_e) => {
            // Returning an error is acceptable for non-replayable bodies
        }
    }
}

// 308 redirects with streaming bodies have the same replay constraints as 307.
#[tokio::test]
async fn redirect_308_streaming_body_should_not_silently_lose_body() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(308)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} body_len={}",
                body_bytes.len()
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("streaming-308-data"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let result = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let body = resp.text().await.unwrap();
            assert!(
                !body.contains("body_len=0") || !body.contains("method=POST"),
                "BUG: 308 redirect with streaming body silently drops the body, got: {body}"
            );
        }
        Err(_) => {
            // Returning an error is acceptable for non-replayable bodies
        }
    }
}
