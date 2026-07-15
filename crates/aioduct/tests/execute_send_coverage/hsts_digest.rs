use super::*;
use http_body_util::BodyExt as _;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 1. HSTS upgrade during execute loop
//    Exercises execute_send.rs:30 (maybe_upgrade_hsts on the original URI)
//    and execute_send.rs:22-36 (maybe_upgrade_hsts implementation).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn hsts_upgrade_prevents_http_request() {
    // Pre-populate HSTS store with a known host
    let store = aioduct::hsts::HstsStore::new();
    let mut sts_headers = http::HeaderMap::new();
    sts_headers.insert(
        http::header::HeaderName::from_static("strict-transport-security"),
        "max-age=31536000".parse().unwrap(),
    );
    store.store_from_response("hsts-host.example.com", &sts_headers);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .hsts(store)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    // Request to http://hsts-host.example.com should be upgraded to https://
    // which will fail because there's no TLS server, but the important thing
    // is that it does NOT hit port 80.
    let result = client
        .get("http://hsts-host.example.com/path")
        .unwrap()
        .send()
        .await;

    // The request should fail (can't connect to port 443 on a non-existent host)
    // but it should NOT be an HttpsOnly error - it should be a connection error
    // because HSTS upgraded the URI to https://.
    assert!(result.is_err());
    let err = result.unwrap_err();
    // Verify it's NOT HttpsOnly (HSTS upgrade happened, it just can't connect)
    assert!(
        !err.to_string().contains("HTTPS only"),
        "HSTS should have upgraded the URI, error should be connection-related, got: {err}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 2. HSTS stores response header during execute loop
//    Exercises execute_send.rs:177-182 (hsts.store_from_response).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(feature = "rustls")]
#[tokio::test]
async fn hsts_stores_sts_header_from_response() {
    install_crypto();

    let store = aioduct::hsts::HstsStore::new();

    // Verify the host is NOT in HSTS store initially
    assert!(
        !store.should_upgrade("127.0.0.1"),
        "HSTS store should not know about 127.0.0.1 initially"
    );

    let (addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("strict-transport-security", "max-age=31536000")
                    .body(Full::new(Bytes::from("secure response")))
                    .unwrap(),
            )
        })
        .await;

    let cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[cert])
        .danger_accept_invalid_hostnames(true)
        .hsts(store.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "secure response");

    // After making HTTPS request, HSTS store should record the host
    assert!(
        store.should_upgrade("127.0.0.1"),
        "HSTS store should record host from Strict-Transport-Security response header"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 3. Digest auth retry with explicit version
//    Exercises execute_send.rs:258-259 (version applied to retry request).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn digest_auth_retry_with_http_version() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="version-test", nonce="version123", qop="auth""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                let version = format!("{:?}", req.version());
                Ok(Response::new(Full::new(Bytes::from(format!(
                    "version={version} auth_present={}",
                    !auth.is_empty()
                )))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("user", "pass")
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Use version(HTTP_11) explicitly to exercise the version path in retry
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .version(http::Version::HTTP_11)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("auth_present=true"),
        "digest auth retry should include authorization, got: {body}"
    );
    // The retry should have used HTTP/1.1 version
    assert!(
        body.contains("version=HTTP/1.1"),
        "digest auth retry should preserve HTTP version, got: {body}"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 4. Digest auth replays the middleware-finalized request without rerunning it.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn digest_auth_does_not_rerun_request_middleware() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();
    let middleware_calls = Arc::new(AtomicU32::new(0));
    let calls = middleware_calls.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.method(), http::Method::POST);
            assert_eq!(req.headers()["x-middleware-retry"], "applied");
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="mw-test", nonce="mwnonce1""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                assert!(req.headers().contains_key(http::header::AUTHORIZATION));
                Ok(Response::new(Full::new(Bytes::from("authenticated"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("user", "pass")
        .middleware(
            move |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    *req.method_mut() = http::Method::POST;
                    req.headers_mut().insert(
                        http::header::HeaderName::from_static("x-middleware-retry"),
                        http::header::HeaderValue::from_static("applied"),
                    );
                } else {
                    *req.method_mut() = http::Method::DELETE;
                    *req.body_mut() = Full::new(Bytes::from_static(b"different request"))
                        .map_err(|never| match never {})
                        .boxed_unsync();
                }
            },
        )
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "authenticated");
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
    assert_eq!(middleware_calls.load(Ordering::SeqCst), 1);
}

#[derive(Clone)]
struct DigestRetryRecorder {
    middleware_attempts: Arc<std::sync::Mutex<Vec<u32>>>,
    observer_attempts: Arc<std::sync::Mutex<Vec<(u32, u32)>>>,
}

impl aioduct::Middleware for DigestRetryRecorder {
    fn on_retry(
        &self,
        _error: &aioduct::Error,
        _uri: &http::Uri,
        _method: &http::Method,
        attempt: u32,
    ) {
        self.middleware_attempts.lock().unwrap().push(attempt);
    }
}

impl aioduct::RequestObserver for DigestRetryRecorder {
    fn on_event(&self, event: &aioduct::RequestEvent) {
        if let aioduct::RequestPhase::Retrying {
            attempt,
            max_retries,
            ..
        } = &event.phase
        {
            self.observer_attempts
                .lock()
                .unwrap()
                .push((*attempt, *max_retries));
        }
    }

    fn on_connection_event(&self, _event: &aioduct::ConnectionEvent) {}
}

#[tokio::test]
async fn digest_and_configured_retries_share_attempts_and_callbacks() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let (addr, _counter) = h1_server_with(move |request| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(match attempt {
                0 => Response::builder()
                    .status(401)
                    .header(
                        http::header::WWW_AUTHENTICATE,
                        r#"Digest realm="events", nonce="events-nonce", qop="auth""#,
                    )
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
                1 => {
                    assert!(request.headers().contains_key(http::header::AUTHORIZATION));
                    Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::new()))
                        .unwrap()
                }
                _ => {
                    assert!(request.headers().contains_key(http::header::AUTHORIZATION));
                    Response::new(Full::new(Bytes::from_static(b"ok")))
                }
            })
        }
    })
    .await;

    let recorder = DigestRetryRecorder {
        middleware_attempts: Arc::new(std::sync::Mutex::new(Vec::new())),
        observer_attempts: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("user", "pass")
        .middleware(recorder.clone())
        .request_observer(recorder.clone())
        .build()
        .unwrap();
    let response = client
        .get(&format!("http://{addr}/digest-events"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(*recorder.middleware_attempts.lock().unwrap(), vec![1, 2]);
    assert_eq!(
        *recorder.observer_attempts.lock().unwrap(),
        vec![(1, 2), (2, 2)]
    );
}

async fn assert_denied_digest_retry_does_not_advance_nonce(denied_retry: aioduct::RetryConfig) {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let (addr, _counter) = h1_server_with(move |request| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if let Some(authorization) = request.headers().get(http::header::AUTHORIZATION) {
                return Ok::<_, Infallible>(Response::new(Full::new(Bytes::copy_from_slice(
                    authorization.as_bytes(),
                ))));
            }
            assert!(attempt < 2, "the third request must be authenticated");
            Ok(Response::builder()
                .status(401)
                .header(
                    http::header::WWW_AUTHENTICATE,
                    r#"Digest realm="denied", nonce="shared-nonce", qop="auth""#,
                )
                .body(Full::new(Bytes::new()))
                .unwrap())
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("user", "pass")
        .build()
        .unwrap();
    let denied = client
        .get(&format!("http://{addr}/digest-denied"))
        .unwrap()
        .retry(denied_retry)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), http::StatusCode::UNAUTHORIZED);

    let authenticated = client
        .get(&format!("http://{addr}/digest-denied"))
        .unwrap()
        .retry(aioduct::RetryConfig::default().max_retries(1))
        .send()
        .await
        .unwrap();
    let authorization = authenticated.text().await.unwrap();
    assert!(
        authorization.contains("nc=00000001"),
        "a denied retry must not reserve a digest nonce count: {authorization}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn digest_retry_denied_by_count_does_not_advance_nonce() {
    assert_denied_digest_retry_does_not_advance_nonce(
        aioduct::RetryConfig::default().max_retries(0),
    )
    .await;
}

#[tokio::test]
async fn digest_retry_denied_by_budget_does_not_advance_nonce() {
    assert_denied_digest_retry_does_not_advance_nonce(
        aioduct::RetryConfig::default()
            .max_retries(1)
            .budget(aioduct::RetryBudget::new(0, 0)),
    )
    .await;
}

#[tokio::test]
async fn digest_header_failure_refunds_the_undispatched_retry_budget() {
    let (addr, counter) = h1_server_with(|_request| async {
        Ok::<_, Infallible>(
            Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .header(
                    http::header::WWW_AUTHENTICATE,
                    r#"Digest realm="refund", nonce="nonce", qop="auth""#,
                )
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;
    let budget = aioduct::RetryBudget::new(1, 0);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("invalid\nusername", "password")
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{addr}/digest-refund"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .budget(budget.clone()),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
    assert_eq!(counter.requests(), 1);
    assert_eq!(budget.available(), 1);
}
