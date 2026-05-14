#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

fn client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build()
}

// ── Redirect Tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn redirect_301_follows_as_get() {
    let (target_addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(301)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} path={path}"
            )))))
        }
    })
    .await;

    let client = client();
    let resp = client
        .post(&format!("http://{target_addr}/redirect"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "301 should downgrade POST to GET, got: {body}"
    );
    assert!(body.contains("path=/final"));
}

#[tokio::test]
async fn redirect_302_follows_as_get() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(302)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} path={path}"
            )))))
        }
    })
    .await;

    let client = client();
    let resp = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "302 should downgrade POST to GET, got: {body}"
    );
}

#[tokio::test]
async fn redirect_307_preserves_method_and_body() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        use http_body_util::BodyExt;
        let path = req.uri().path().to_string();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(307)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} path={path} body={}",
                String::from_utf8_lossy(&body_bytes)
            )))))
        }
    })
    .await;

    let client = client();
    let resp = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body("payload")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=POST"),
        "307 should preserve POST method, got: {body}"
    );
    assert!(
        body.contains("body=payload"),
        "307 should preserve body, got: {body}"
    );
}

#[tokio::test]
async fn redirect_cross_origin_strips_auth() {
    // Start the target server first so we know its address
    let (target_addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "auth={auth}"
        )))))
    })
    .await;

    // Origin server redirects to the target server (different port = cross-origin)
    let (origin_addr, _) = aioduct_test_server::h1::h1_server_with(move |req| {
        let target_addr = target_addr;
        async move {
            let path = req.uri().path();
            if path == "/redirect" {
                let resp = Response::builder()
                    .status(302)
                    .header("Location", format!("http://{target_addr}/final"))
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                Ok::<_, Infallible>(resp)
            } else {
                Ok(Response::new(Full::new(Bytes::from("origin"))))
            }
        }
    })
    .await;

    let client = client();
    let resp = client
        .get(&format!("http://{origin_addr}/redirect"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("secret-token"),
        "cross-origin redirect should strip Authorization, got: {body}"
    );
}

#[tokio::test]
async fn redirect_same_origin_keeps_auth() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(302)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let auth = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "auth={auth}"
            )))))
        }
    })
    .await;

    let client = client();
    let resp = client
        .get(&format!("http://{addr}/redirect"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Bearer secret-token"),
        "same-origin redirect should preserve auth, got: {body}"
    );
}

#[tokio::test]
async fn redirect_policy_none_returns_redirect_response() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(302)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            Ok(Response::new(Full::new(Bytes::from("final"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::None)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/redirect"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 302);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/final"
    );
}

#[tokio::test]
async fn redirect_limited_policy_stops_at_limit() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path.starts_with("/r") && path.len() < 10 {
            let resp = Response::builder()
                .status(302)
                .header("Location", format!("{path}x"))
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            Ok(Response::new(Full::new(Bytes::from("final"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::Limited(2))
        .timeout(Duration::from_secs(5))
        .build();

    let result = client
        .get(&format!("http://{addr}/r"))
        .unwrap()
        .send()
        .await;

    match result {
        Ok(resp) => {
            assert_ne!(
                resp.status(),
                200,
                "should not reach final destination with limited(2) and >2 redirects"
            );
        }
        Err(e) => {
            assert!(e.is_redirect(), "should be a redirect error, got: {e}");
        }
    }
}

// ── HTTPS-Only Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn https_only_rejects_http() {
    let (addr, _) = aioduct_test_server::h1::h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .https_only(true)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "https_only should reject http:// URLs");
}

// ── Cookie Jar Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn cookie_jar_stores_and_sends_cookies() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            let resp = Response::builder()
                .status(200)
                .header("Set-Cookie", "session=abc123; Path=/")
                .body(Full::new(Bytes::from("cookie set")))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "cookie={cookie}"
            )))))
        }
    })
    .await;

    let jar = aioduct::cookie::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("session=abc123"),
        "cookie jar should send stored cookie, got: {body}"
    );
}

// ── Read Timeout Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn read_timeout_on_slow_body() {
    let (addr, _) =
        aioduct_test_server::h1::h1_slow_body_server(100, Duration::from_millis(500)).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .read_timeout(Duration::from_millis(200))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let result = resp.bytes().await;
    assert!(
        result.is_err(),
        "read_timeout should trigger on slow chunked body"
    );
}

// ── Decompression Tests ────────────────────────────────────────────────

#[cfg(feature = "gzip")]
#[tokio::test]
async fn decompression_gzip() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let (addr, _) = aioduct_test_server::h1::h1_server_with(|_req| async {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"hello compressed world").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("Content-Encoding", "gzip")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    })
    .await;

    let client = client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello compressed world");
}

// ── Middleware Tests ───────────────────────────────────────────────────

#[tokio::test]
async fn middleware_modifies_request() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-custom")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "x-custom={custom}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBoxBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-custom"),
                    http::header::HeaderValue::from_static("injected"),
                );
            },
        )
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("x-custom=injected"),
        "middleware should inject header, got: {body}"
    );
}

// ── Form POST Test ─────────────────────────────────────────────────────

#[tokio::test]
async fn form_post_sends_urlencoded() {
    let (addr, _) = aioduct_test_server::h1::h1_echo_server().await;

    let client = client();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .form(&[("key", "value"), ("foo", "bar")])
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("key=value"),
        "form POST should send urlencoded body, got: {body}"
    );
}

// ── JSON POST Test ─────────────────────────────────────────────────────

#[cfg(feature = "json")]
#[tokio::test]
async fn json_post_sends_json() {
    let (addr, _) = aioduct_test_server::h1::h1_echo_server().await;

    let client = client();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .json(&serde_json::json!({"key": "value"}))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(r#""key":"value""#) || body.contains(r#""key": "value""#),
        "json POST should send JSON body, got: {body}"
    );
}
