#![cfg(feature = "tokio")]

use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

/// Test 1: user-supplied Transfer-Encoding header is stripped.
///
/// The user sets `Transfer-Encoding: chunked` on a request with a buffered
/// body. The server echoes back the received headers. Verify the server did
/// NOT receive the `Transfer-Encoding: chunked` header (hyper strips it
/// because it manages framing internally), and that the body was sent
/// correctly.
#[tokio::test]
async fn user_supplied_transfer_encoding_is_stripped() {
    let (addr, _counter) = h1_server_with(|req| async move {
        // Check that Transfer-Encoding was NOT received
        let has_te = req.headers().contains_key("transfer-encoding");
        let te_value = req
            .headers()
            .get("transfer-encoding")
            .map(|v| v.to_str().unwrap_or("").to_string());

        let body_bytes = http_body_util::BodyExt::collect(req.into_body())
            .await
            .unwrap()
            .to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);

        let report = format!("has_te={has_te} te_value={:?} body={body_str}", te_value);
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(report))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .header_str("transfer-encoding", "chunked")
        .unwrap()
        .body("hello chunked")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    // Verify the server did NOT receive Transfer-Encoding: chunked
    assert!(
        body.contains("has_te=false"),
        "Transfer-Encoding should have been stripped by hyper, got: {body}"
    );
    // Verify the body was sent correctly
    assert!(
        body.contains("body=hello chunked"),
        "body should be 'hello chunked', got: {body}"
    );
}

/// Test 2: duplicate Content-Length headers - body is still sent correctly.
///
/// The user sets two Content-Length headers: one via `header()` (insert
/// overwrites) and then another via `headers()` (extend). Verify the body
/// was sent correctly (one Content-Length was used, not corrupt framing).
#[tokio::test]
async fn duplicate_content_length_request_sent_correctly() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let body_bytes = http_body_util::BodyExt::collect(req.into_body())
            .await
            .unwrap()
            .to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body_str.to_string()))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    // First set Content-Length via header() (insert — overwrites any existing)
    // Then set another via headers() (extend — appends)
    let mut extra_headers = http::HeaderMap::new();
    extra_headers.insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from_static("5"),
    );

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_static("10"),
        )
        .headers(extra_headers)
        .body("hello")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "hello",
        "body should be 'hello' despite duplicate Content-Length, got: {body}"
    );
}

/// Test 3: oversized header value is either rejected at build time or sent
/// correctly without truncation.
///
/// The user sets a very large (~100KB) header value via `header_str`. It
/// must either be rejected (Http::Error at build time) or sent correctly to
/// the server. The key assertion: it must not silently truncate the header.
#[tokio::test]
async fn oversized_header_value_rejected() {
    let large_value = "X".repeat(100 * 1024);

    // We use a custom handler that echoes back the received x-large-header
    // value so we can verify it wasn't truncated.
    let large_value_clone = large_value.clone();
    let (addr, _counter) = h1_server_with(move |req| {
        let large_value = large_value_clone.clone();
        async move {
            // Check whether the server received the large header
            let received = req
                .headers()
                .get("x-large-header")
                .map(|v| v.as_bytes().to_vec());

            match received {
                Some(received_bytes) => {
                    let received_len = received_bytes.len();
                    let matches = received_bytes == large_value.as_bytes();
                    let body = format!("received=true len={received_len} matches={matches}");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
                }
                None => {
                    // The header was not received at all — it was silently
                    // dropped.
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("received=false"))))
                }
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    // Try to set the large header
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("x-large-header", &large_value);

    match result {
        Ok(builder) => {
            // The header was accepted at build time. Now send the request
            // and verify the server received the header correctly (not
            // truncated).
            let resp = builder.send().await.unwrap();
            let body = resp.text().await.unwrap();
            // The header must either be received in full or rejected at
            // build time. Silently dropping it is a bug.
            assert!(
                body.contains("received=true") && body.contains("matches=true"),
                "oversized header was silently dropped or truncated.\n\
                 response body: {body}\n\
                 expected header len: {}",
                large_value.len(),
            );
        }
        Err(e) => {
            // The header was rejected at build time — this is acceptable
            // behavior as long as it was not silently truncated.
            let err_str = format!("{e:?}");
            assert!(
                err_str.contains("Http")
                    || err_str.contains("header")
                    || err_str.contains("InvalidHeader")
                    || err_str.contains("invalid"),
                "oversized header was rejected but with unexpected error: {err_str}"
            );
        }
    }
}
