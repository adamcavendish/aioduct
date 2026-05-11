#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_bearer_auth() {
    let addr = start_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .bearer_auth("my-secret-token")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "Bearer my-secret-token");
}
#[tokio::test]
async fn test_basic_auth() {
    let addr = start_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .basic_auth("user", Some("pass"))
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "Basic dXNlcjpwYXNz");
}
#[tokio::test]
async fn test_digest_auth_flow() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: challenge with Digest auth
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="test@example.com", nonce="dcd98b7102dd2f0e", qop="auth""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                // Second request: verify Authorization header is present
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                assert!(auth.starts_with("Digest "), "expected Digest auth, got: {auth}");
                assert!(auth.contains("username=\"testuser\""));
                assert!(auth.contains("realm=\"test@example.com\""));
                assert!(auth.contains("qop=auth"));
                Ok(Response::new(Full::new(Bytes::from("authenticated"))))
            }
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .digest_auth("testuser", "testpass")
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "authenticated");
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_digest_auth_post_replays_buffered_body() {
    use http_body_util::BodyExt;

    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            let method = req.method().clone();
            let auth = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_else(|| "none".to_owned());
            let body = req.into_body().collect().await.unwrap().to_bytes();

            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="post@example.com", nonce="abcdef123456", qop="auth""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                let body = format!(
                    "method={method}\nauth={auth}\nbody={}",
                    String::from_utf8_lossy(&body)
                );
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .digest_auth("testuser", "testpass")
        .build();

    let resp = client
        .post(&format!("http://{addr}/submit"))
        .unwrap()
        .body("payload=aioduct")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=POST"),
        "POST method must be replayed: {body}"
    );
    assert!(
        body.contains("auth=Digest "),
        "digest retry must include Authorization: {body}"
    );
    assert!(
        body.contains("body=payload=aioduct"),
        "digest retry must replay the original buffered request body: {body}"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_digest_auth_no_challenge() {
    let addr = start_server().await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .digest_auth("user", "pass")
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}
