use super::*;

#[tokio::test]
async fn response_body_limit_notifies_rejection() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let observer: RejectionObserver = Arc::new(move |reason| {
        observed.lock().expect("observer lock").push(reason);
    });
    let body: HyperIncomingBody = Full::new(Bytes::from_static(b"abcd"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let err = ResponseLimitBody::new_policy(body, Some(2), None, Some(observer))
        .collect()
        .await
        .expect_err("response body should exceed limit");
    assert!(matches!(err, ErrorCode::HttpResponseBodySize(Some(2))));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::BodyLimit]);
}

#[tokio::test]
async fn deadline_body_notifies_rejection() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let observer: RejectionObserver = Arc::new(move |reason| {
        observed.lock().expect("observer lock").push(reason);
    });
    let body: HyperIncomingBody = Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let err = DeadlineBody::new(
        body,
        Instant::now() - Duration::from_millis(1),
        Some(observer),
    )
    .collect()
    .await
    .expect_err("deadline should expire");
    assert!(matches!(err, ErrorCode::HttpResponseTimeout));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
}

#[tokio::test]
async fn deadline_body_wakes_pending_body() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let observer: RejectionObserver = Arc::new(move |reason| {
        observed.lock().expect("observer lock").push(reason);
    });
    let err = DeadlineBody::new(
        pending_incoming_body(),
        Instant::now() + Duration::from_millis(10),
        Some(observer),
    )
    .collect()
    .await
    .expect_err("deadline should wake stalled response body");
    assert!(matches!(err, ErrorCode::HttpResponseTimeout));
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
}

#[tokio::test]
async fn wasmtime_body_wrapper_preserves_host_deadline_mapping() {
    let reasons = Arc::new(Mutex::new(Vec::new()));
    let observed = reasons.clone();
    let policy = ExactOriginPolicy::new("http://example.com")
        .expect("policy should build")
        .deadline(Instant::now() + Duration::from_millis(10))
        .on_rejection(move |reason| {
            observed.lock().expect("observer lock").push(reason);
        });
    let host = WasiHttpHost::builder()
        .transport(PendingResponseTransport)
        .policy(policy)
        .build()
        .expect("host should build");
    let cfg = config(false);
    let guest_between_bytes_timeout = cfg.between_bytes_timeout;
    let incoming = host
        .send_inner(request("http://example.com/".to_string()), cfg)
        .await
        .expect("host should return response headers");

    assert_eq!(incoming.between_bytes_timeout, guest_between_bytes_timeout);

    let IncomingResponse {
        resp,
        worker,
        between_bytes_timeout,
    } = incoming;
    let mut body = HostIncomingBody::new(resp.into_body(), between_bytes_timeout);
    if let Some(worker) = worker {
        body.retain_worker(worker);
    }
    let mut stream = body.take_stream().expect("body stream should be available");
    stream.ready().await;
    let err = stream.read(1).expect_err("deadline should surface");
    match err {
        StreamError::LastOperationFailed(error) => {
            assert!(matches!(
                error.downcast_ref::<ErrorCode>(),
                Some(ErrorCode::HttpResponseTimeout)
            ));
        }
        other => panic!("expected last operation failure, got {other:?}"),
    }
    let captured = reasons.lock().expect("observer lock");
    assert_eq!(captured.as_slice(), &[RejectionReason::Deadline]);
}

#[test]
fn wasi_body_error_mapping_preserves_error_code() {
    let err = map_wasi_body_error(ErrorCode::ConnectionWriteTimeout);
    assert!(matches!(
        map_aioduct_error(err),
        ErrorCode::ConnectionWriteTimeout
    ));
}

#[tokio::test]
async fn wasi_body_error_mapping_preserves_hyper_wrapped_error() {
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

    let policy = ExactOriginPolicy::new(&format!("http://{addr}")).expect("policy should build");
    let host = test_host(policy);
    let req = hyper::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}/"))
        .body(failing_body(ErrorCode::ConnectionWriteTimeout))
        .expect("request should build");
    let err = host
        .send_inner(req, config(false))
        .await
        .expect_err("request body error should be preserved");
    assert!(matches!(err, ErrorCode::ConnectionWriteTimeout));
}
