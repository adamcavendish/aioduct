use super::*;

/// A per-request `no_decompression()` call returns the raw compressed body and
/// suppresses the Accept-Encoding request header, overriding the client default.
#[cfg(feature = "gzip")]
#[tokio::test]
async fn per_request_no_decompression_returns_raw_bytes() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(b"raw gzip payload").unwrap();
    let compressed = encoder.finish().unwrap();
    let expected = compressed.clone();

    let captured_accept = Arc::new(Mutex::new(None::<String>));
    let cap = captured_accept.clone();
    let (addr, _counter) = h1_server_with(move |req: Request<hyper::body::Incoming>| {
        let cap = cap.clone();
        let body = compressed.clone();
        async move {
            *cap.lock().unwrap() = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap_or("").to_string());
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .no_decompression()
        .send()
        .await
        .unwrap();

    // Content-Encoding is preserved and the body is the raw gzip bytes.
    assert_eq!(resp.headers().get("content-encoding").unwrap(), "gzip");
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(bytes.as_ref(), expected.as_slice());

    // No Accept-Encoding was sent for this request.
    assert!(
        captured_accept.lock().unwrap().is_none(),
        "no_decompression() must suppress the Accept-Encoding header"
    );
}

/// Without no_decompression(), the same client still decompresses (sanity that
/// the override is per-request, not global).
#[cfg(feature = "gzip")]
#[tokio::test]
async fn no_decompression_is_per_request() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let make_body = || {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"decoded text").unwrap();
        encoder.finish().unwrap()
    };
    let (addr, _counter) = h1_server_with(move |_req| {
        let body = make_body();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-encoding", "gzip")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    // Default request: decompressed.
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "decoded text");

    // Same client, no_decompression request: raw bytes (not equal to plaintext).
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .no_decompression()
        .send()
        .await
        .unwrap();
    let raw = resp.bytes().await.unwrap();
    assert_ne!(raw.as_ref(), b"decoded text");
}

/// Accept-Encoding never advertises `br` when the brotli feature is disabled.
#[cfg(all(feature = "gzip", not(feature = "brotli")))]
#[tokio::test]
async fn accept_encoding_omits_br_without_brotli_feature() {
    let (addr, _counter) = h1_server_with(|req: Request<hyper::body::Incoming>| async move {
        let accept = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let accept = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !accept.contains("br"),
        "must not advertise br without brotli, got: {accept}"
    );
}
