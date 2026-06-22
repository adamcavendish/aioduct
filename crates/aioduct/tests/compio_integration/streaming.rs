use super::*;

#[test]
fn test_compio_streaming_body_request() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let body_bytes = req.collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "received:{}",
            body_str
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();

        // Create a streaming body (non-buffered) -- this exercises the
        // RequestBody::Streaming branch at execute_local.rs line 58
        let stream_body: aioduct::body::RequestBodySend =
            http_body_util::Full::new(Bytes::from("streaming-payload"))
                .map_err(|never| match never {})
                .boxed_unsync();

        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .body_stream(stream_body)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "received:streaming-payload");
    });
}

#[test]
fn test_compio_streaming_body_not_replayable_on_redirect() {
    // Streaming bodies cannot be replayed after a redirect, so the second
    // request after redirect should have no body (or the redirect should work
    // with method change to GET).
    let final_addr = start_server_with_tokio(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    });

    let redirect_addr = start_server_with_tokio(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(303) // 303 changes method to GET
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();

        let stream_body: aioduct::body::RequestBodySend =
            http_body_util::Full::new(Bytes::from("stream-data"))
                .map_err(|never| match never {})
                .boxed_unsync();

        let resp = client
            .post_local(&format!("http://{redirect_addr}/"))
            .unwrap()
            .body_stream(stream_body)
            .send()
            .await
            .unwrap();

        // After 303 redirect, method should change to GET
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "GET");
    });
}

#[test]
fn test_compio_bytes_stream_exposes_trailers() {
    use std::io::Write;
    // Raw chunked response with a trailer, served from a std-thread TCP server.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        tx.send(addr).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        // Drain the request line/headers.
        use std::io::Read;
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: x-checksum\r\n\r\n",
        );
        let _ = stream.write_all(b"5\r\nhello\r\n");
        let _ = stream.write_all(b"0\r\nx-checksum: abc123\r\n\r\n");
        let _ = stream.flush();
    });
    let addr = rx.recv().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let mut stream = resp.into_bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(body, b"hello");
        let trailers = stream.trailers().expect("trailers should be captured");
        assert_eq!(trailers.get("x-checksum").unwrap(), "abc123");
    });
}
