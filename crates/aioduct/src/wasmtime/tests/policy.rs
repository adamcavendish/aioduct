use super::*;

#[test]
fn exact_origin_rejects_path() {
    assert!(matches!(
        ExactOriginPolicy::new("https://example.com/path"),
        Err(PolicyError::OriginMustNotContainPath)
    ));
}

#[test]
fn builder_rejects_forbidden_injected_header() {
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .inject_header(http::header::HOST, HeaderValue::from_static("example.com"));
    assert!(matches!(
        WasiHttpHost::builder().policy(policy).build(),
        Err(BuildError::Policy(PolicyError::InjectedForbiddenHeader(_)))
    ));
}

#[test]
fn builder_rejects_invalid_denied_header_prefixes() {
    for prefix in ["", "bad header"] {
        let policy = ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .deny_header_prefix(prefix);
        match WasiHttpHost::builder()
            .transport(CollectingTransport)
            .policy(policy)
            .build()
        {
            Err(BuildError::Policy(PolicyError::InvalidDeniedHeaderPrefix(value))) => {
                assert_eq!(value, prefix);
            }
            Ok(_) => panic!("expected invalid denied prefix error, got host"),
            Err(other) => panic!("expected invalid denied prefix error, got {other}"),
        }
    }
}

#[cfg(any(
    all(not(feature = "tokio"), feature = "smol"),
    all(not(feature = "tokio"), not(feature = "smol"), feature = "compio")
))]
#[test]
fn builder_requires_explicit_transport_without_tokio_default() {
    let policy = ExactOriginPolicy::new("http://example.com").expect("policy should build");
    assert!(matches!(
        WasiHttpHost::builder().policy(policy).build(),
        Err(BuildError::MissingTransport)
    ));
}

#[tokio::test]
async fn origin_mismatch_is_denied_before_transport() {
    let policy = ExactOriginPolicy::new("http://127.0.0.1:1").expect("policy should build");
    let host = test_host(policy);
    let err = host
        .send_inner(request("http://127.0.0.1:2/".into()), config(false))
        .await
        .expect_err("origin mismatch should be rejected");
    assert!(matches!(err, ErrorCode::HttpRequestDenied));
}

#[tokio::test]
async fn rejection_observer_receives_low_cardinality_reason() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new("http://127.0.0.1:1")
        .expect("policy should build")
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let host = test_host(policy);
    let err = host
        .send_inner(request("http://127.0.0.1:2/".into()), config(false))
        .await
        .expect_err("origin mismatch should be rejected");
    assert!(matches!(err, ErrorCode::HttpRequestDenied));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::OriginMismatch]);
    assert_eq!(captured[0].as_str(), "origin_mismatch");
}

#[tokio::test]
async fn forbidden_sensitive_header_is_denied() {
    let (policy, reasons) = record_rejections(
        ExactOriginPolicy::new("http://127.0.0.1:1")
            .expect("policy should build")
            .forbid_sensitive_headers(),
    );
    let host = test_host(policy);
    let req = hyper::Request::builder()
        .uri("http://127.0.0.1:1/")
        .header(AUTHORIZATION, "Bearer guest")
        .body(empty_body())
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("sensitive header should be rejected");
    assert!(matches!(err, ErrorCode::HttpRequestDenied));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::ProtectedHeader]);
}

#[tokio::test]
async fn exact_denied_header_is_denied_before_transport() {
    let (policy, reasons) = record_rejections(
        ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .deny_header(HeaderName::from_static("x-denied")),
    );
    let host = WasiHttpHost::builder()
        .transport(PanickingTransport)
        .policy(policy)
        .build()
        .expect("host should build");
    let req = hyper::Request::builder()
        .uri("http://example.com/")
        .header("x-denied", "guest")
        .body(empty_body())
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("denied header should be rejected");

    assert!(matches!(err, ErrorCode::HttpRequestDenied));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::DeniedHeader]);
    assert_eq!(captured[0].as_str(), "denied_header");
}

#[tokio::test]
async fn batch_denied_header_is_denied_before_transport() {
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .deny_headers([
            HeaderName::from_static("x-one"),
            HeaderName::from_static("x-two"),
        ]);
    let host = WasiHttpHost::builder()
        .transport(PanickingTransport)
        .policy(policy)
        .build()
        .expect("host should build");
    let req = hyper::Request::builder()
        .uri("http://example.com/")
        .header("x-two", "guest")
        .body(empty_body())
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("batch denied header should be rejected");

    assert!(matches!(err, ErrorCode::HttpRequestDenied));
}

#[tokio::test]
async fn denied_prefix_rejects_forwarded_header() {
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .deny_header_prefix("x-forwarded-");
    let host = WasiHttpHost::builder()
        .transport(PanickingTransport)
        .policy(policy)
        .build()
        .expect("host should build");
    let header = HeaderName::from_bytes(b"X-Forwarded-For").expect("header should parse");
    let req = hyper::Request::builder()
        .uri("http://example.com/")
        .header(header, "203.0.113.7")
        .body(empty_body())
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("denied prefix should reject header");

    assert!(matches!(err, ErrorCode::HttpRequestDenied));
}

#[test]
fn denied_prefix_match_is_case_insensitive() {
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .deny_header_prefix("X-FoRwArDeD-");
    assert!(policy.is_denied_request_header(&HeaderName::from_static("x-forwarded-host")));
    WasiHttpHost::builder()
        .transport(PanickingTransport)
        .policy(policy)
        .build()
        .expect("host should build");
}

#[test]
fn denied_headers_are_forbidden_for_wasmtime_field_construction() {
    let mut host = WasiHttpHost::builder()
        .transport(PanickingTransport)
        .policy(
            ExactOriginPolicy::new("http://example.com")
                .expect("policy should build")
                .deny_header(HeaderName::from_static("x-denied"))
                .deny_header_prefix("x-forwarded-"),
        )
        .build()
        .expect("host should build");

    assert!(WasiHttpHooks::is_forbidden_header(
        &mut host,
        &HeaderName::from_static("x-denied")
    ));
    assert!(WasiHttpHooks::is_forbidden_header(
        &mut host,
        &HeaderName::from_static("x-forwarded-for")
    ));
    assert!(!WasiHttpHooks::is_forbidden_header(
        &mut host,
        &HeaderName::from_static("x-forwardedness")
    ));
}

#[tokio::test]
async fn batch_denied_prefix_rejects_matching_header() {
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .deny_header_prefixes(["proxy-", "x-denied-"]);
    let host = WasiHttpHost::builder()
        .transport(PanickingTransport)
        .policy(policy)
        .build()
        .expect("host should build");
    let req = hyper::Request::builder()
        .uri("http://example.com/")
        .header("x-denied-test", "guest")
        .body(empty_body())
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("batch denied prefix should reject header");

    assert!(matches!(err, ErrorCode::HttpRequestDenied));
}

#[tokio::test]
async fn denied_request_trailer_is_rejected() {
    let (policy, reasons) = record_rejections(
        ExactOriginPolicy::new("http://example.com")
            .expect("policy should build")
            .deny_header(HeaderName::from_static("x-denied-trailer")),
    );
    let host = WasiHttpHost::builder()
        .transport(CollectingTransport)
        .policy(policy)
        .build()
        .expect("host should build");

    let mut trailers = HeaderMap::new();
    trailers.insert(
        "x-denied-trailer",
        HeaderValue::from_static("guest-trailer"),
    );
    let req = hyper::Request::builder()
        .method(http::Method::POST)
        .uri("http://example.com/")
        .body(request_trailers_body(trailers))
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("denied request trailer should be rejected");

    assert!(matches!(err, ErrorCode::HttpRequestDenied));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::DeniedHeader]);
}

#[tokio::test]
async fn nonmatching_denied_headers_still_pass() {
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .deny_headers([FORWARDED])
        .deny_header_prefixes(["x-forwarded-", "proxy-"]);
    let host = WasiHttpHost::builder()
        .transport(CollectingTransport)
        .policy(policy)
        .build()
        .expect("host should build");

    let mut trailers = HeaderMap::new();
    trailers.insert("x-forwardedness", HeaderValue::from_static("ok"));
    let req = hyper::Request::builder()
        .method(http::Method::POST)
        .uri("http://example.com/")
        .header("x-forwardedness", "ok")
        .body(request_trailers_body(trailers))
        .expect("request should build");
    let incoming = host
        .send_inner(req, config(false))
        .await
        .expect("nonmatching request should pass");

    assert_eq!(incoming.resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn host_injects_secret_header_after_validation() {
    let response = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
    let (addr, seen) = raw_server(response).await;
    let policy = ExactOriginPolicy::new(&format!("http://{addr}"))
        .expect("policy should build")
        .forbid_sensitive_headers()
        .inject_header(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
    let host = test_host(policy);
    let incoming = host
        .send_inner(request(format!("http://{addr}/")), config(false))
        .await
        .expect("request should succeed");
    assert_eq!(incoming.resp.status(), http::StatusCode::OK);
    let text = seen.await.expect("server should capture request");
    let text = text.to_ascii_lowercase();
    assert!(text.contains(&format!("host: {addr}")));
    assert!(text.contains("authorization: bearer secret"));
}
