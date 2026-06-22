use super::*;

#[test]
fn test_compio_chunk_download_with_ranges() {
    let addr = start_server_with_tokio(|req| async move {
        let data = b"abcdefghijklmnopqrstuvwxyz";
        match req.method() {
            &http::Method::HEAD => Ok::<_, Infallible>(
                Response::builder()
                    .header("accept-ranges", "bytes")
                    .header("content-length", data.len().to_string())
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ),
            _ => {
                if let Some(range) = req.headers().get("range") {
                    let range_str = range.to_str().unwrap_or("");
                    let range_str = range_str.strip_prefix("bytes=").unwrap_or(range_str);
                    let parts: Vec<&str> = range_str.split('-').collect();
                    let start: usize = parts[0].parse().unwrap_or(0);
                    let end: usize = parts[1].parse().unwrap_or(data.len() - 1);
                    let slice = &data[start..=end];
                    Ok(Response::builder()
                        .status(206)
                        .body(Full::new(Bytes::from(slice.to_vec())))
                        .unwrap())
                } else {
                    Ok(Response::new(Full::new(Bytes::from(data.to_vec()))))
                }
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/"))
            .chunks(2)
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 26);
        assert_eq!(&result.data[..], b"abcdefghijklmnopqrstuvwxyz");
    });
}

#[test]
fn test_compio_chunk_download_fallback_no_ranges() {
    let addr = start_server_with_tokio(|req| async move {
        match req.method() {
            &http::Method::HEAD => Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "11")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ),
            _ => Ok(Response::new(Full::new(Bytes::from("hello world")))),
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/"))
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 11);
        assert_eq!(&result.data[..], b"hello world");
    });
}

#[test]
fn test_compio_chunk_download_local_fallback_no_ranges() {
    let addr = start_server_with_tokio(|req| async move {
        if req.method() == http::Method::HEAD {
            // No accept-ranges header
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "13")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/file"))
            .chunks(4)
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 13);
        assert_eq!(&result.data[..], b"hello aioduct");
    });
}

#[test]
fn test_compio_chunk_download_local_with_ranges() {
    use std::sync::Arc;

    let body_data: Vec<u8> = (0..200u8).cycle().take(1000).collect();
    let body_data_arc = Arc::new(body_data.clone());

    let addr = start_server_with_tokio(move |req| {
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
                    .body(Full::new(Bytes::copy_from_slice(slice)))
                    .unwrap());
            }
            Ok(Response::new(Full::new(Bytes::from(
                body_data.as_ref().to_vec(),
            ))))
        }
    });

    let body_data_check = body_data.clone();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/file"))
            .chunks(4)
            .download()
            .await
            .unwrap();

        assert_eq!(result.total_size, 1000);
        assert_eq!(result.data.len(), 1000);
        assert_eq!(&result.data[..], &body_data_check[..]);
    });
}

#[test]
fn test_compio_chunk_download_head_failure() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let result = client
            .chunk_download_local(&format!("http://{addr}/missing"))
            .download()
            .await;
        assert!(result.is_err(), "HEAD failure should return error");
    });
}
