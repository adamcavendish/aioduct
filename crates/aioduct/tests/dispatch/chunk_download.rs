use super::*;

// ── 76. Chunk download with range support ────────────────────────────────────

#[tokio::test]
async fn chunk_download_with_range_support() {
    // Server that supports Accept-Ranges and serves partial content
    let body_data: Vec<u8> = (0..200u8).cycle().take(1000).collect();
    let body_data_arc = Arc::new(body_data.clone());

    let (addr, _counter) = h1_server_with(move |req| {
        let body_data = body_data_arc.clone();
        async move {
            if req.method() == http::Method::HEAD {
                return Ok::<_, Infallible>(
                    Response::builder()
                        .header("accept-ranges", "bytes")
                        .header("content-length", body_data.len().to_string())
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                );
            }

            if let Some(range) = req.headers().get("range") {
                let range_str = range.to_str().unwrap();
                let range_str = range_str.strip_prefix("bytes=").unwrap();
                let parts: Vec<&str> = range_str.split('-').collect();
                let start: usize = parts[0].parse().unwrap();
                let end: usize = parts[1].parse().unwrap();
                let slice = &body_data[start..=end];
                return Ok(Response::builder()
                    .status(206)
                    .header(
                        "content-range",
                        format!("bytes {start}-{end}/{}", body_data.len()),
                    )
                    .body(Full::new(Bytes::copy_from_slice(slice)))
                    .unwrap());
            }

            Ok(Response::new(Full::new(Bytes::from(
                body_data.as_ref().to_vec(),
            ))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(result.total_size, 1000);
    assert_eq!(result.data.len(), 1000);
    assert_eq!(&result.data[..], &body_data[..]);
}

// ── 77. Chunk download fallback without range support ────────────────────────

#[tokio::test]
async fn chunk_download_fallback_no_ranges() {
    let (addr, _counter) = h1_server_with(|req| async move {
        if req.method() == http::Method::HEAD {
            // No accept-ranges header
            return Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "13")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            );
        }
        Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let result = client
        .chunk_download(&format!("http://{addr}/file"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(result.total_size, 13);
    assert_eq!(result.data, Bytes::from("hello aioduct"));
}

// ── 78. Chunk download HEAD fails returns error ──────────────────────────────

#[tokio::test]
async fn chunk_download_head_failure_returns_error() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let result = client
        .chunk_download(&format!("http://{addr}/missing"))
        .download()
        .await;

    assert!(result.is_err(), "HEAD failure should propagate error");
}
