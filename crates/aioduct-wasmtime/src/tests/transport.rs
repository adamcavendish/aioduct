use super::*;

#[tokio::test]
async fn native_forward_deadline_timeout_notifies_rejection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let addr = listener.local_addr().expect("listener should have address");
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0_u8; 1024];
        let _ = stream.read(&mut buf).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await;
    });

    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
        .expect("policy should build")
        .deadline(Instant::now() + Duration::from_millis(10))
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let host = test_host(policy);
    let err = host
        .send_inner(request(format!("http://{addr}/")), config(false))
        .await
        .expect_err("host deadline should time out native forward");
    assert!(matches!(
        err,
        ErrorCode::HttpResponseTimeout | ErrorCode::ConnectionReadTimeout
    ));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
}

#[tokio::test]
async fn deadline_upload_timeout_notifies_rejection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let addr = listener.local_addr().expect("listener should have address");
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0_u8; 1024];
        let _ = stream.read(&mut buf).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
        .expect("policy should build")
        .deadline(Instant::now() + Duration::from_millis(10))
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let host = test_host(policy);
    let req = hyper::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}/"))
        .body(pending_body())
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("host deadline should time out stalled upload");
    assert!(matches!(
        err,
        ErrorCode::HttpResponseTimeout | ErrorCode::ConnectionWriteTimeout
    ));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
}

#[tokio::test]
async fn first_byte_timeout_maps_to_connection_read_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let addr = listener.local_addr().expect("listener should have address");
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0_u8; 1024];
        let _ = stream.read(&mut buf).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await;
    });

    let policy = ExactOriginPolicy::new(&format!("http://{addr}")).expect("policy should build");
    let host = test_host(policy);
    let mut cfg = config(false);
    cfg.first_byte_timeout = Duration::from_millis(10);
    let err = host
        .send_inner(request(format!("http://{addr}/")), cfg)
        .await
        .expect_err("first byte should time out");
    assert!(matches!(err, ErrorCode::ConnectionReadTimeout));
}

#[cfg(feature = "smol")]
#[tokio::test]
async fn smol_transport_services_wasi_http_request() {
    let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
    let (addr, seen) = raw_server(response).await;
    let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
        .expect("policy should build")
        .inject_header(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer smol-secret"),
        );
    let transport = aioduct::SmolClient::builder()
        .build()
        .expect("smol transport should build");
    let host = WasiHttpHost::builder()
        .transport(transport)
        .policy(policy)
        .build()
        .expect("host should build");
    let incoming = host
        .send_inner(request(format!("http://{addr}/")), config(false))
        .await
        .expect("smol transport should forward request");
    assert_eq!(incoming.resp.status(), http::StatusCode::OK);
    let text = seen.await.expect("server should capture request");
    assert!(
        text.to_ascii_lowercase()
            .contains("authorization: bearer smol-secret")
    );
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn compio_transport_services_wasi_http_request() {
    let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
    let (addr, seen) = raw_server(response).await;
    let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
        .expect("policy should build")
        .inject_header(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer compio-secret"),
        );
    let transport = CompioHostTransport::new().expect("compio host transport should start");
    let host = WasiHttpHost::builder()
        .transport(transport)
        .policy(policy)
        .build()
        .expect("host should build");
    let incoming = host
        .send_inner(request(format!("http://{addr}/")), config(false))
        .await
        .expect("compio transport should forward request");
    assert_eq!(incoming.resp.status(), http::StatusCode::OK);
    let text = seen.await.expect("server should capture request");
    assert!(
        text.to_ascii_lowercase()
            .contains("authorization: bearer compio-secret")
    );
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn custom_ca_allows_self_signed_tls() {
    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let cert = aioduct::Certificate::from_der(cert_der.to_vec());
    let transport = aioduct::TokioClient::builder()
        .add_root_certificates(&[cert])
        .build()
        .expect("transport should build");
    let policy =
        ExactOriginPolicy::new(&format!("https://localhost:{}", addr.port())).expect("policy");
    let host = WasiHttpHost::builder()
        .transport(transport)
        .policy(policy)
        .build()
        .expect("host should build");
    let incoming = host
        .send_inner(
            request(format!("https://localhost:{}/", addr.port())),
            config(true),
        )
        .await
        .expect("custom CA should trust self-signed server");
    assert_eq!(incoming.resp.status(), http::StatusCode::OK);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn insecure_transport_allows_self_signed_tls() {
    let (addr, _cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let transport = aioduct::TokioClient::builder().danger_accept_invalid_certs();
    let policy =
        ExactOriginPolicy::new(&format!("https://localhost:{}", addr.port())).expect("policy");
    let host = WasiHttpHost::builder()
        .transport_builder(transport)
        .policy(policy)
        .build()
        .expect("host should build");
    let incoming = host
        .send_inner(
            request(format!("https://localhost:{}/", addr.port())),
            config(true),
        )
        .await
        .expect("insecure mode should accept self-signed server");
    assert_eq!(incoming.resp.status(), http::StatusCode::OK);
}
