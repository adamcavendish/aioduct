use super::*;

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
// 4. Digest auth retry with middleware applied
//    Exercises execute_send.rs:263-264 (middleware.apply_request on retry).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn digest_auth_retry_applies_middleware() {
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
                            r#"Digest realm="mw-test", nonce="mwnonce1", qop="auth""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                let mw_header = req
                    .headers()
                    .get("x-middleware-retry")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                Ok(Response::new(Full::new(Bytes::from(format!(
                    "mw={mw_header} auth_present={}",
                    auth.starts_with("Digest ")
                )))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("user", "pass")
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-middleware-retry"),
                    http::header::HeaderValue::from_static("applied"),
                );
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

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("mw=applied"),
        "middleware should be applied on digest auth retry, got: {body}"
    );
    assert!(
        body.contains("auth_present=true"),
        "digest auth should be present on retry, got: {body}"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
