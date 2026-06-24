use super::*;

#[tokio::test]
async fn test_read_timeout_does_not_apply_to_headers() {
    // Note: aioduct's read_timeout only applies to body reads, not header wait.
    // Use request timeout for header wait timeouts.
    let (addr, _counter) = h1_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow headers"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "slow headers");
}

#[tokio::test]
async fn test_read_timeout_applies_to_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = stream.write_all(b"world").await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body_result = resp.text().await;
    assert!(
        body_result.is_err(),
        "read_timeout should fire on slow body chunks"
    );
    let err = body_result.unwrap_err();
    assert!(
        matches!(err, aioduct::Error::ReadTimeout),
        "slow body chunk should be classified as ReadTimeout, got: {err:?}"
    );
}

#[tokio::test]
async fn read_timeout_waits_until_slow_upload_finishes() {
    let (addr, _counter) = h1_server_with(|req| async {
        use http_body_util::BodyExt;

        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "uploaded={} bytes",
            body.len()
        )))))
    })
    .await;

    let chunks = vec![
        Bytes::from_static(b"slow-"),
        Bytes::from_static(b"request-"),
        Bytes::from_static(b"body"),
    ];
    let stream = futures_util::stream::unfold((0, chunks), |(idx, chunks)| async move {
        if idx == chunks.len() {
            None
        } else {
            tokio::time::sleep(Duration::from_millis(75)).await;
            let frame = hyper::body::Frame::data(chunks[idx].clone());
            Some((Ok::<_, aioduct::Error>(frame), (idx + 1, chunks)))
        }
    });

    use http_body_util::BodyExt;
    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::StreamBody::new(stream).boxed_unsync();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(50))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .post(&format!("http://{addr}/upload"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "uploaded=17 bytes");
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn compressed_body_read_timeout_is_not_reported_as_decode_error() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(b"compressed response body that will stall before the gzip stream finishes")
        .unwrap();
    let compressed = encoder.finish().unwrap();
    let first_fragment_len = compressed.len() / 2;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;

        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            compressed.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream
            .write_all(&compressed[..first_fragment_len])
            .await
            .unwrap();
        stream.flush().await.unwrap();

        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(100))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/compressed"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let err = resp.bytes().await.unwrap_err();
    assert!(
        matches!(err, aioduct::Error::ReadTimeout),
        "stalled compressed body should be ReadTimeout, not a decode error: {err:?}"
    );
}

#[tokio::test]
async fn test_read_timeout_allows_slow_but_steady_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        for i in 0..3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let chunk = format!("1\r\n{i}\r\n");
            stream.write_all(chunk.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        }

        stream.write_all(b"0\r\n\r\n").await.unwrap();
        stream.flush().await.unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .read_timeout(Duration::from_millis(200))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "012", "slow-but-within-threshold body should succeed");
}

#[tokio::test]
async fn test_content_length_preserved_through_timeout() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "5")
                .body(Full::new(Bytes::from("hello")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(5));
}
