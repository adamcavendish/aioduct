#![cfg(feature = "tokio")]

mod common;
use common::*;

#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_decompression() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"hello compressed world").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "gzip")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(!resp.headers().contains_key("content-encoding"));
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello compressed world");
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_accept_encoding_header() {
    let handler = |req: Request<hyper::body::Incoming>| async move {
        let accept = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept))))
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::new();
    let text = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(text.contains("gzip"));
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_no_decompression_passthrough() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"raw gzip data").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "gzip")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::builder().no_decompression().build();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(resp.headers().contains_key("content-encoding"));
    let bytes = resp.bytes().await.unwrap();
    // Should be raw gzip, not decompressed
    assert_ne!(bytes.as_ref(), b"raw gzip data");
}
#[cfg(feature = "deflate")]
#[tokio::test]
async fn test_deflate_decompression() {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"deflate test payload").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "deflate")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::new();
    let text = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert_eq!(text, "deflate test payload");
}
#[tokio::test]
async fn test_get_no_content_headers() {
    let addr = start_server_with(|req| async move {
        assert_eq!(req.method(), "GET");
        assert!(
            req.headers().get("content-length").is_none(),
            "GET should not have content-length"
        );
        assert!(
            req.headers().get("content-type").is_none(),
            "GET should not have content-type"
        );
        assert!(
            req.headers().get("transfer-encoding").is_none(),
            "GET should not have transfer-encoding"
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_empty_body_head_request() {
    let addr = start_server_with(|req| async move {
        assert_eq!(req.method(), "HEAD");
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-encoding", "gzip")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .head(&format!("http://{addr}/gzip"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "");
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_custom_accept_encoding_preserved() {
    let addr = start_server_with(|req| async move {
        let accept_encoding = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept_encoding))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::ACCEPT_ENCODING,
            http::header::HeaderValue::from_static("identity"),
        )
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "identity");
}
