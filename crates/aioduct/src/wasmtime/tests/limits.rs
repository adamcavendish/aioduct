use super::*;

#[tokio::test]
async fn response_header_limit_is_enforced() {
    let response = b"HTTP/1.1 200 OK\r\nx-large: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\ncontent-length: 0\r\n\r\n";
    let (addr, _seen) = raw_server(response).await;
    let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
        .expect("policy should build")
        .header_limit(64);
    let host = test_host(policy);
    let err = host
        .send_inner(request(format!("http://{addr}/")), config(false))
        .await
        .expect_err("response headers should exceed limit");
    assert!(matches!(
        err,
        ErrorCode::HttpResponseHeaderSectionSize(Some(64))
    ));
}

#[tokio::test]
async fn request_trailer_injected_header_is_denied() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .inject_header(AUTHORIZATION, HeaderValue::from_static("Bearer secret"))
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let host = WasiHttpHost::builder()
        .transport(CollectingTransport)
        .policy(policy)
        .build()
        .expect("host should build");

    let mut trailers = HeaderMap::new();
    trailers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer guest"));
    let req = hyper::Request::builder()
        .method(http::Method::POST)
        .uri("http://example.com/")
        .body(request_trailers_body(trailers))
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("injected header trailer should be rejected");

    assert!(matches!(err, ErrorCode::HttpRequestDenied));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::ProtectedHeader]);
}

#[tokio::test]
async fn request_trailer_header_limit_is_enforced() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .header_limit(64)
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let host = WasiHttpHost::builder()
        .transport(CollectingTransport)
        .policy(policy)
        .build()
        .expect("host should build");

    let mut trailers = HeaderMap::new();
    trailers.insert(
        "x-large",
        HeaderValue::from_static(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    );
    let req = hyper::Request::builder()
        .method(http::Method::POST)
        .uri("http://example.com/")
        .body(request_trailers_body(trailers))
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("oversized request trailers should be rejected");

    assert!(matches!(
        err,
        ErrorCode::HttpRequestTrailerSectionSize(Some(64))
    ));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::HeaderLimit]);
}

#[tokio::test]
async fn response_trailer_header_limit_is_enforced() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .header_limit(64)
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "x-large",
        HeaderValue::from_static(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    );
    let host = WasiHttpHost::builder()
        .transport(TrailerResponseTransport { trailers })
        .policy(policy)
        .build()
        .expect("host should build");

    let incoming = host
        .send_inner(request("http://example.com/".into()), config(false))
        .await
        .expect("response headers should succeed");
    let err = incoming
        .resp
        .into_body()
        .collect()
        .await
        .expect_err("oversized response trailers should be rejected");

    assert!(matches!(
        err,
        ErrorCode::HttpResponseTrailerSectionSize(Some(64))
    ));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::HeaderLimit]);
}

#[tokio::test]
async fn request_body_limit_is_mapped_to_wasi_error() {
    let response = b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n";
    let (addr, seen) = raw_server(response).await;
    let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
        .expect("policy should build")
        .body_limit(2);
    let host = test_host(policy);
    let req = hyper::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}/"))
        .body(full_body(b"abcd"))
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("request body should exceed limit");
    match err {
        ErrorCode::HttpRequestBodySize(Some(2)) => {}
        other => panic!("expected request body limit error, got {other:?}"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), seen)
            .await
            .is_err(),
        "known oversized body should be rejected before opening upstream connection"
    );
}

#[tokio::test]
async fn streaming_request_body_limit_notifies_rejection() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .body_limit(2)
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let body: crate::body::RequestBodySend = full_body(b"abcd")
        .map_err(map_wasi_body_error)
        .boxed_unsync();
    let err = RequestLimitBody::new_policy(body, policy.body_limit, &policy)
        .collect()
        .await
        .expect_err("request body should exceed limit");
    assert!(request_body_limit_from_error(&err).is_some());
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::BodyLimit]);
}
