#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{h1_server, h1_server_with};

#[tokio::test]
async fn test_bearer_auth() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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
    let (addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    let (addr, _counter) = h1_server_with(move |req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("testuser", "testpass")
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
}
#[tokio::test]
async fn test_digest_auth_post_replays_buffered_body() {
    use http_body_util::BodyExt;

    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("testuser", "testpass")
        .build()
        .unwrap();

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
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("user", "pass")
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// BUG: digest_auth.rs:53-55 always uses md5_hex regardless of the algorithm parameter.
// When the server requests algorithm=SHA-256, the client still computes MD5 hashes,
// causing authentication to fail.
#[tokio::test]
async fn digest_auth_sha256_should_not_use_md5() {
    use std::time::Duration;

    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Challenge with SHA-256 algorithm
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="sha256@example.com", nonce="sha256nonce123", qop="auth", algorithm=SHA-256"#,
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

                // Check if the response hash length indicates SHA-256 (64 hex chars)
                // vs MD5 (32 hex chars)
                let has_sha256_response = if let Some(start) = auth.find("response=\"") {
                    let hash_start = start + 10;
                    if let Some(end) = auth[hash_start..].find('"') {
                        let hash = &auth[hash_start..hash_start + end];
                        hash.len() == 64 // SHA-256 produces 64 hex chars
                    } else {
                        false
                    }
                } else {
                    false
                };

                let has_algorithm = auth.contains("algorithm=SHA-256");

                let body = format!(
                    "sha256_hash={has_sha256_response}\nalgorithm_present={has_algorithm}\nauth={auth}"
                );
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("testuser", "testpass")
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();

    assert!(
        body.contains("sha256_hash=true"),
        "BUG: digest_auth.rs:53-55 always uses md5_hex() regardless of algorithm. \
         When server requests algorithm=SHA-256, response hash should be 64 hex chars (SHA-256), \
         not 32 (MD5). Response: {body}"
    );
}

// BUG: digest_auth.rs:57 uses `q.contains("auth")` which matches "auth-int" too.
// When the server sends qop="auth-int", the client incorrectly treats it as qop="auth".
#[tokio::test]
async fn digest_auth_qop_auth_int_not_confused_with_auth() {
    use std::time::Duration;

    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Challenge with qop="auth-int" ONLY (not "auth")
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="qop@example.com", nonce="qopnonce123", qop="auth-int""#,
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

                // Check if the client claims qop=auth or qop=auth-int
                let claims_auth = auth.contains("qop=auth,") || auth.contains("qop=auth\n") || auth.ends_with("qop=auth");
                let claims_auth_int = auth.contains("qop=auth-int");

                let body = format!(
                    "claims_auth={claims_auth}\nclaims_auth_int={claims_auth_int}\nauth={auth}"
                );
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .digest_auth("testuser", "testpass")
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();

    // The client should NOT claim qop=auth when the server only offered auth-int.
    // auth-int requires HA2 = MD5(method:uri:body_hash), which is different from
    // auth's HA2 = MD5(method:uri).
    assert!(
        !body.contains("claims_auth=true") || body.contains("claims_auth_int=true"),
        "BUG: digest_auth.rs:57 uses contains(\"auth\") which matches \"auth-int\". \
         Client claims qop=auth when server only offered qop=auth-int. \
         Response: {body}"
    );
}
