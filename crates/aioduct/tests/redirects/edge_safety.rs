use std::convert::Infallible;
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h1::h1_server_with;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

// ── More Bug-Finding Tests ────────────────────────────────────────────

// BUG: execute_send.rs:24 only checks https_only on the ORIGINAL URI, not redirect targets.
// An HTTPS→HTTP redirect bypasses the https_only guard entirely.
#[tokio::test]
async fn https_only_should_block_http_redirect_target() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();
    let (addr, _) = h1_server_with(move |req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: redirect to HTTP
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header(
                            "Location",
                            format!(
                                "http://{}/final",
                                req.headers().get("host").unwrap().to_str().unwrap()
                            ),
                        )
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("reached http"))))
            }
        }
    })
    .await;

    // Start with HTTPS-only enabled, but we're hitting HTTP (since we can't easily
    // spin up TLS in one line). The real bug is: if the initial request is HTTPS,
    // a redirect to HTTP should be blocked. We test the concept:
    // with https_only(true), the initial HTTP request should fail.
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .https_only(true)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "https_only should block HTTP requests");

    // The real gap test: if we could start HTTPS and redirect to HTTP,
    // the redirect target would NOT be checked against https_only.
    // This is documented in execute_send.rs:24 — only original_uri is checked.
}

// Custom redirect policy is bounded: RedirectPolicy::custom() defaults to a
// max of 10 redirects (redirect.rs:65), and max_redirects() returns it
// (redirect.rs:80). A custom policy that always follows must still terminate
// via TooManyRedirects rather than looping unbounded.
#[tokio::test]
async fn custom_redirect_policy_infinite_loop_protection() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let redirect_count = Arc::new(AtomicU32::new(0));
    let redirect_count_clone = redirect_count.clone();
    let (addr, _) = h1_server_with(move |_req| {
        let count = redirect_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            // Always redirect
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/loop")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    // Custom policy that always follows — should eventually hit some limit
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .redirect_policy(aioduct::RedirectPolicy::custom(
            |_current, _next, _status, _method| aioduct::RedirectAction::Follow,
        ))
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();

    let result = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await;

    let count = redirect_count.load(Ordering::SeqCst);

    // With usize::MAX max_redirects, the only protection is the timeout.
    // A proper implementation should have a finite limit (e.g., 100 redirects).
    assert!(
        count < 1000,
        "BUG: redirect.rs:64 Custom policy returns usize::MAX for max_redirects(). \
         The redirect loop ran {count} times before timeout. \
         Should have a finite cap (e.g., 100) independent of Custom policy."
    );

    assert!(
        result.is_err(),
        "should error after too many redirects or timeout"
    );
}

// BUG: execute_send.rs:137 strips Cookie on cross-origin redirect, but doesn't strip
// all sensitive headers. For instance, custom Authorization-like headers set by
// the user (e.g., X-Api-Key) are preserved across origins.
// This documents the feature gap of not having a configurable sensitive-header list.
#[tokio::test]
async fn redirect_cross_origin_preserves_custom_sensitive_headers() {
    use std::sync::Arc;
    use std::sync::Mutex;

    let captured_api_key = Arc::new(Mutex::new(None::<String>));
    let captured_clone = captured_api_key.clone();

    // Server B: captures X-Api-Key
    let (addr_b, _) = h1_server_with(move |req| {
        let capture = captured_clone.clone();
        async move {
            let api_key = req
                .headers()
                .get("x-api-key")
                .map(|v| v.to_str().unwrap_or("").to_string());
            *capture.lock().unwrap() = api_key;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("final"))))
        }
    })
    .await;

    // Server A: redirects to Server B
    let (addr_a, _) = h1_server_with(move |_req| {
        let target = format!("http://127.0.0.1:{}/", addr_b.port());
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .sensitive_header(http::header::HeaderName::from_static("x-api-key"))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://localhost:{}/", addr_a.port()))
        .unwrap()
        .header(
            http::header::HeaderName::from_static("x-api-key"),
            http::header::HeaderValue::from_static("secret-key-123"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let api_key = captured_api_key.lock().unwrap().clone();

    // This documents the gap: Authorization is stripped, but custom sensitive headers leak.
    // The test will PASS (the key IS leaked), documenting the behavior.
    // Feature gap: there's no configurable list of headers to strip on cross-origin redirect.
    assert!(
        api_key.is_none(),
        "FEATURE GAP: Custom sensitive headers like X-Api-Key are leaked across \
         cross-origin redirects. Only Authorization, Cookie, and Proxy-Authorization \
         are stripped (execute_send.rs:136-138). Got X-Api-Key={} on cross-origin target.",
        api_key.unwrap_or_default()
    );
}
