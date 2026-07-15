use super::*;

// ── Forwarding tests ────────────────────────────────────────────────

#[test]
fn test_compio_forward_basic_get() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "upstream:{path}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/hello/world")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "upstream:/hello/world");
    });
}

#[test]
fn test_compio_forward_strip_prefix() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/api/v1/users")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .strip_prefix("/api/v1")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "/users");
    });
}

#[test]
fn test_compio_forward_preserve_host() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .header("host", "original.example.com")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .preserve_host()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "original.example.com");
    });
}

#[test]
fn test_compio_forward_preserve_host_uses_uri_authority_without_host() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let host = req
            .headers()
            .get(http::header::HOST)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("http://original.example.com/resource")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{upstream_addr}"))
            .preserve_host()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "original.example.com");
    });
}

#[test]
fn test_compio_forward_invalid_te_is_protocol_dependent() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        assert!(!req.headers().contains_key(http::header::TE));
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .uri("/resource")
            .header(http::header::TE, "gzip")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{upstream_addr}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
    });
}

#[test]
fn test_compio_forward_strips_hop_by_hop_request_trailers() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let collected = req.into_body().collect().await.unwrap();
        let trailers = collected.trailers().unwrap();
        assert!(!trailers.contains_key(http::header::CONNECTION));
        assert!(!trailers.contains_key("x-hop-secret"));
        let checksum = trailers
            .get("x-checksum")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(checksum))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-checksum", http::HeaderValue::from_static("preserved"));
        trailers.insert("x-hop-secret", http::HeaderValue::from_static("remove"));
        let body = http_body_util::StreamBody::new(futures_util::stream::iter([
            Ok::<_, Infallible>(http_body::Frame::data(Bytes::from_static(b"body"))),
            Ok(http_body::Frame::trailers(trailers)),
        ]));
        let incoming = http::Request::builder()
            .method("POST")
            .uri("/upload")
            .header(http::header::CONTENT_LENGTH, "4")
            .header(http::header::CONNECTION, "x-hop-secret")
            .header(http::header::TRAILER, "x-checksum, x-hop-secret")
            .body(body)
            .unwrap();

        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{upstream_addr}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "preserved");
    });
}

#[test]
fn test_compio_forward_unknown_length_get_body_uses_h11_framing() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        assert_eq!(req.method(), http::Method::GET);
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(body)))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let body =
            http_body_util::StreamBody::new(futures_util::stream::iter([Ok::<_, Infallible>(
                http_body::Frame::data(Bytes::from_static(b"get-body")),
            )]));
        let incoming = http::Request::builder()
            .method(http::Method::GET)
            .uri("/stream")
            .body(body)
            .unwrap();

        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{upstream_addr}"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "get-body");
    });
}

#[test]
fn test_compio_forward_strips_hop_by_hop_response_trailers() {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new().unwrap().block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0, "client closed before request headers completed");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Checksum\r\n\r\n4\r\nbody\r\n0\r\nConnection: x-hop-secret\r\nX-Hop-Secret: remove\r\nX-Checksum: preserved\r\n\r\n",
                )
                .await
                .unwrap();
        });
    });
    let upstream_addr = addr_rx.recv().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .uri("/response-trailers")
            .header(http::header::CONNECTION, "te")
            .header(http::header::TE, "trailers")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{upstream_addr}"))
            .send()
            .await
            .unwrap();

        let mut body = Vec::new();
        let mut stream = response.into_bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.unwrap());
        }
        let trailers = stream.trailers().unwrap();
        assert!(!trailers.contains_key(http::header::CONNECTION));
        assert!(!trailers.contains_key("x-hop-secret"));
        assert_eq!(trailers.get("x-checksum").unwrap(), "preserved");
        assert_eq!(body, b"body");
    });
}

#[test]
fn test_compio_forward_extra_header() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let val = req
            .headers()
            .get("x-forwarded-for")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .header(
                http::header::HeaderName::from_static("x-forwarded-for"),
                http::header::HeaderValue::from_static("10.0.0.1"),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "10.0.0.1");
    });
}
