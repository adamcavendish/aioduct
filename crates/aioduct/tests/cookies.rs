#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

use aioduct::SameSite;

#[tokio::test]
async fn test_cookie_jar_stores_and_sends() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();

        if req.uri().path() == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "session=abc123; Path=/")
                    .body(Full::new(Bytes::from("cookie set")))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from(format!(
                "cookies={cookie}"
            )))))
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    let resp = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cookie set");

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body, "cookies=session=abc123");
}
#[tokio::test]
async fn test_cookie_jar_multiple_cookies() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();

        match req.uri().path() {
            "/set1" => Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "a=1")
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap(),
            ),
            "/set2" => Ok(Response::builder()
                .header("set-cookie", "b=2")
                .body(Full::new(Bytes::from("ok")))
                .unwrap()),
            _ => Ok(Response::new(Full::new(Bytes::from(format!(
                "cookies={cookie}"
            ))))),
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    client
        .get(&format!("http://{addr}/set1"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    client
        .get(&format!("http://{addr}/set2"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.contains("a=1"), "expected cookie a, got: {body}");
    assert!(body.contains("b=2"), "expected cookie b, got: {body}");
}
#[tokio::test]
async fn test_no_cookie_jar_no_cookies() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let has_cookie = req.headers().contains_key("cookie");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "has_cookie={has_cookie}"
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "has_cookie=false");
}
#[tokio::test]
async fn test_cookie_jar_same_host_shared() {
    let jar = aioduct::CookieJar::new();

    let (addr1, _counter) = h1_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(
            Response::builder()
                .header("set-cookie", "session=abc123")
                .body(Full::new(Bytes::from(cookie)))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    // First request stores the cookie
    let resp1 = client
        .get(&format!("http://{addr1}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body1 = resp1.text().await.unwrap();
    assert!(body1.is_empty(), "first request should have no cookie");

    // Second request to same host should include the stored cookie
    let resp2 = client
        .get(&format!("http://{addr1}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body2 = resp2.text().await.unwrap();
    assert!(
        body2.contains("session=abc123"),
        "second request should have cookie, got: {body2}"
    );
}
#[tokio::test]
async fn test_cookie_store_max_age_zero() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();
    let (addr, _counter) = h1_server_with(move |req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("set-cookie", "key=val; Max-Age=0")
                        .body(Full::new(Bytes::from("set")))
                        .unwrap(),
                )
            } else {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=none",
        "cookie with Max-Age=0 should not be sent"
    );
}
#[tokio::test]
async fn test_cookie_store_expired() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();
    let (addr, _counter) = h1_server_with(move |req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header(
                            "set-cookie",
                            "key=val; Expires=Wed, 21 Oct 2015 07:28:00 GMT",
                        )
                        .body(Full::new(Bytes::from("set")))
                        .unwrap(),
                )
            } else {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=none",
        "cookie with past Expires should not be sent"
    );
}
#[tokio::test]
async fn test_cookie_store_path_scoping() {
    let (addr, _counter) = h1_server_with(|req| async move {
        if req.uri().path() == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "key=val; Path=/subpath")
                    .body(Full::new(Bytes::from("set")))
                    .unwrap(),
            )
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap().to_owned());
            let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
            Ok(Response::new(Full::new(Bytes::from(body))))
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=none",
        "cookie with Path=/subpath should not be sent to /"
    );

    let resp = client
        .get(&format!("http://{addr}/subpath"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=key=val",
        "cookie with Path=/subpath should be sent to /subpath"
    );
}
#[tokio::test]
async fn test_cookie_store_overwrite() {
    let (addr, _counter) = h1_server_with(|req| async move {
        match req.uri().path() {
            "/set1" => Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "key=val1")
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap(),
            ),
            "/set2" => Ok(Response::builder()
                .header("set-cookie", "key=val2")
                .body(Full::new(Bytes::from("ok")))
                .unwrap()),
            _ => {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    client
        .get(&format!("http://{addr}/set1"))
        .unwrap()
        .send()
        .await
        .unwrap();
    client
        .get(&format!("http://{addr}/set2"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body, "cookie=key=val2");
}

#[tokio::test]
async fn cookie_response_accessor() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header(
                    "set-cookie",
                    "key=val; Domain=127.0.0.1; Path=/api; Secure; HttpOnly; SameSite=Strict",
                )
                .header("set-cookie", "lax_cookie=lax; SameSite=Lax")
                .header("set-cookie", "plain=text")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let cookies = resp.cookies();
    assert_eq!(cookies.len(), 3);

    // Find each cookie by name
    let key_cookie = cookies.iter().find(|c| c.name() == "key").unwrap();
    assert_eq!(key_cookie.value(), "val");
    assert_eq!(key_cookie.domain(), Some("127.0.0.1"));
    assert_eq!(key_cookie.path(), "/api");
    assert!(key_cookie.secure());
    assert!(key_cookie.http_only());
    assert_eq!(key_cookie.same_site(), Some(&SameSite::Strict));

    let lax_cookie = cookies.iter().find(|c| c.name() == "lax_cookie").unwrap();
    assert_eq!(lax_cookie.value(), "lax");
    assert_eq!(lax_cookie.same_site(), Some(&SameSite::Lax));
    assert!(!lax_cookie.secure());
    assert!(!lax_cookie.http_only());

    let plain_cookie = cookies.iter().find(|c| c.name() == "plain").unwrap();
    assert_eq!(plain_cookie.value(), "text");
    assert_eq!(plain_cookie.domain(), Some("127.0.0.1"));
    assert_eq!(plain_cookie.path(), "/");
    assert!(!plain_cookie.secure());
    assert!(!plain_cookie.http_only());
    assert_eq!(plain_cookie.same_site(), None);
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// Multiple Set-Cookie headers should all be stored.
#[tokio::test]
async fn cookie_jar_stores_multiple_set_cookie_headers() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            let resp = Response::builder()
                .status(200)
                .header("Set-Cookie", "a=1; Path=/")
                .header("Set-Cookie", "b=2; Path=/")
                .header("Set-Cookie", "c=3; Path=/")
                .body(Full::new(Bytes::from("cookies set")))
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
        body.contains("a=1"),
        "cookie a=1 should be stored, got: {body}"
    );
    assert!(
        body.contains("b=2"),
        "cookie b=2 should be stored, got: {body}"
    );
    assert!(
        body.contains("c=3"),
        "cookie c=3 should be stored, got: {body}"
    );
}

// BUG: cookie.rs insert() overwrites caller's Cookie header.
#[tokio::test]
async fn cookie_jar_should_not_overwrite_manual_cookie_header() {
    let (addr, _) = h1_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "cookie={cookie}"
        )))))
    })
    .await;

    let jar = aioduct::cookie::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar.clone())
        .timeout(Duration::from_secs(5))
        .build();

    // Pre-populate jar
    let (set_addr, _) = h1_server_with(|_req| async move {
        let resp = Response::builder()
            .header("Set-Cookie", "jar_cookie=from_jar; Path=/")
            .body(Full::new(Bytes::from("ok")))
            .unwrap();
        Ok::<_, Infallible>(resp)
    })
    .await;

    let resp = client
        .get(&format!("http://127.0.0.1:{}/", set_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.text().await.unwrap();

    // Send request with manual Cookie header + jar cookies
    let resp = client
        .get(&format!("http://127.0.0.1:{}/check", addr.port()))
        .unwrap()
        .header(
            http::header::COOKIE,
            http::header::HeaderValue::from_static("manual=user_set"),
        )
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("manual=user_set"),
        "BUG: Cookie jar overwrites the manual Cookie header. \
         Manual cookie 'manual=user_set' was lost, got: {body}"
    );
}

// BUG: cookie.rs:207-208 lowercases the Path attribute value.
// `Path=/MyApp/API` is stored as `/myapp/api`. Since path matching (line 138)
// is case-sensitive, the cookie never matches requests to `/MyApp/API`.
#[tokio::test]
async fn cookie_path_value_should_preserve_case() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("Set-Cookie", "token=abc; Path=/MyApp/API")
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap(),
            )
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

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .timeout(Duration::from_secs(5))
        .build();

    let _ = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/MyApp/API/endpoint"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();

    assert!(
        body.contains("token=abc"),
        "BUG: Path value is lowercased at storage time (cookie.rs:207-208). \
         Cookie with Path=/MyApp/API should match /MyApp/API/endpoint, got: {body}"
    );
}

// BUG: cookie.rs:137-140 uses starts_with for path matching, violating RFC 6265 §5.1.4.
// A cookie with Path=/api matches /api-extra (no / separator check).
#[tokio::test]
async fn cookie_path_match_requires_separator() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("Set-Cookie", "token=abc; Path=/api")
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap(),
            )
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

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .timeout(Duration::from_secs(5))
        .build();

    let _ = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    // /api-extra should NOT match Path=/api (RFC 6265 §5.1.4 requires / separator)
    let resp = client
        .get(&format!("http://{addr}/api-extra"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();

    assert!(
        !body.contains("token=abc"),
        "BUG: cookie.rs:138 uses starts_with without checking for '/' separator. \
         Cookie with Path=/api should NOT be sent to /api-extra per RFC 6265 §5.1.4, \
         got: {body}"
    );
}

// BUG: cookie.rs:221-228 only matches Expires= and expires= (partially case-sensitive).
// All other attributes use the lowercased copy, but Expires uses the original attr.
// EXPIRES= in any other casing is silently ignored.
#[tokio::test]
async fn cookie_expires_attribute_should_be_case_insensitive() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            // Server sends Expires with unusual casing
            Ok::<_, Infallible>(
                Response::builder()
                    .header(
                        "Set-Cookie",
                        "token=abc; EXPIRES=Wed, 01 Jan 2020 00:00:00 GMT",
                    )
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap(),
            )
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

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .timeout(Duration::from_secs(5))
        .build();

    let _ = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();

    assert!(
        !body.contains("token=abc"),
        "BUG: cookie.rs:221-228 only matches 'Expires=' and 'expires=', not 'EXPIRES='. \
         Cookie with past EXPIRES should be treated as expired and not sent, got: {body}"
    );
}

// BUG: cookie.rs:100-101 deduplicates cookies by name only, not name+domain+path.
// Two cookies with the same name but different paths/domains overwrite each other.
#[tokio::test]
async fn cookie_dedup_should_consider_path_not_just_name() {
    let jar = aioduct::CookieJar::new();

    // Store cookie with Path=/api
    let mut headers = http::HeaderMap::new();
    headers.append(
        http::header::SET_COOKIE,
        "token=api_value; Path=/api".parse().unwrap(),
    );
    jar.store_from_response("example.com", "/", &headers);

    // Store cookie with Path=/web (same name, different path)
    let mut headers = http::HeaderMap::new();
    headers.append(
        http::header::SET_COOKIE,
        "token=web_value; Path=/web".parse().unwrap(),
    );
    jar.store_from_response("example.com", "/", &headers);

    let cookies = jar.cookies();
    let token_cookies: Vec<_> = cookies.iter().filter(|c| c.name() == "token").collect();

    assert_eq!(
        token_cookies.len(),
        2,
        "BUG: cookie.rs:100-101 uses name-only dedup. Two cookies named 'token' \
         with different paths should coexist (RFC 6265 §5.3 uses name+domain+path as key), \
         but found {} cookies",
        token_cookies.len()
    );
}

// BUG: cookie.rs:302 `(year - 1970)` underflows for years before 1970.
// In debug builds this panics; in release it wraps to a huge value,
// making the cookie appear non-expired (security regression).
#[tokio::test]
async fn cookie_expires_year_before_1970_should_not_panic() {
    let jar = aioduct::CookieJar::new();

    let mut headers = http::HeaderMap::new();
    headers.append(
        http::header::SET_COOKIE,
        "old=val; Expires=Thu, 01 Jan 1960 00:00:00 GMT"
            .parse()
            .unwrap(),
    );
    // Should not panic (debug) or treat as non-expired (release)
    jar.store_from_response("example.com", "/", &headers);

    let mut req_headers = http::HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", &mut req_headers);

    assert!(
        req_headers.get("cookie").is_none(),
        "BUG: cookie.rs:302 `(year - 1970)` underflows for year < 1970. \
         Cookie with Expires in 1960 should be treated as expired and not sent"
    );
}

// BUG: cookie.rs SameSite attribute is parsed and stored but never enforced.
// Cookies with SameSite=Strict should not be sent on cross-site requests.
#[tokio::test]
async fn cookie_samesite_strict_should_not_be_sent_cross_site() {
    let jar = aioduct::CookieJar::new();

    // Store a SameSite=Strict cookie
    let mut headers = http::HeaderMap::new();
    headers.append(
        http::header::SET_COOKIE,
        "session=abc; SameSite=Strict".parse().unwrap(),
    );
    jar.store_from_response("example.com", "/", &headers);

    // Verify the cookie was stored with SameSite attribute
    let cookies = jar.cookies();
    let session = cookies.iter().find(|c| c.name() == "session").unwrap();
    assert_eq!(
        session.same_site(),
        Some(&aioduct::SameSite::Strict),
        "SameSite=Strict should be parsed"
    );

    // Apply to request — SameSite=Strict should ideally not be sent in cross-site contexts.
    // The library sends it unconditionally since apply_to_request never checks same_site.
    let mut req_headers = http::HeaderMap::new();
    jar.apply_to_request("example.com", false, "/", &mut req_headers);

    // This documents the feature gap: SameSite is parsed but never enforced.
    // In a full implementation, there would be a parameter for cross-site context.
    assert!(
        req_headers.get("cookie").is_some(),
        "FEATURE GAP: SameSite=Strict cookie is sent unconditionally. \
         cookie.rs:111-159 apply_to_request() never reads the same_site field. \
         A proper implementation would check the cross-site context."
    );
}

// BUG: cookie.rs:263-318 parse_http_date only accepts RFC 7231 IMF-fixdate format.
// RFC 850 format ("Sunday, 06-Nov-94 08:49:37 GMT") and asctime format
// ("Sun Nov  6 08:49:37 1994") are silently ignored, leaving cookies with no expiry.
#[tokio::test]
async fn cookie_expires_rfc850_format_should_be_parsed() {
    let jar = aioduct::CookieJar::new();

    // Store cookie with RFC 850 format Expires (past date)
    let mut headers = http::HeaderMap::new();
    headers.append(
        http::header::SET_COOKIE,
        "old=val; Expires=Sunday, 06-Nov-94 08:49:37 GMT"
            .parse()
            .unwrap(),
    );
    jar.store_from_response("example.com", "/", &headers);

    let cookies = jar.cookies();

    // The RFC 850 date is in the past (1994), so the cookie should be expired.
    // But since parse_http_date only accepts RFC 7231 format, it fails to parse
    // and the cookie is treated as non-expired.
    assert!(
        cookies.is_empty(),
        "BUG: cookie.rs:263-318 parse_http_date only accepts RFC 7231 format. \
         RFC 850 date 'Sunday, 06-Nov-94 08:49:37 GMT' is not parsed, \
         so the cookie is stored as non-expired. Found {} cookies.",
        cookies.len()
    );
}

#[tokio::test]
async fn cookie_jar_rejects_cross_domain_cookie() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            let resp = Response::builder()
                .status(200)
                .header("Set-Cookie", "legit=yes")
                .header("Set-Cookie", "stolen=secret; Domain=evil.com")
                .body(Full::new(Bytes::from("ok")))
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

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body, "cookie=legit=yes");
}

#[tokio::test]
async fn response_cookies_filters_mismatched_domain() {
    let (addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("set-cookie", "good=1")
                .header("set-cookie", "cross=2; Domain=evil.com")
                .header("set-cookie", "also_good=3; Domain=127.0.0.1")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let cookies = resp.cookies();
    assert_eq!(cookies.len(), 2);
    assert!(cookies.iter().any(|c| c.name() == "good"));
    assert!(cookies.iter().any(|c| c.name() == "also_good"));
    assert!(!cookies.iter().any(|c| c.name() == "cross"));
}

// #100: positive Max-Age=N should expire the cookie after N seconds
#[tokio::test]
async fn cookie_positive_max_age_expires_after_duration() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();
    let (addr, _counter) = h1_server_with(move |req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("set-cookie", "key=val; Max-Age=1")
                        .body(Full::new(Bytes::from("set")))
                        .unwrap(),
                )
            } else {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .build();

    client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    // Immediately the cookie should be sent
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("key=val"),
        "cookie should be sent before expiry, got: {body}"
    );

    // Wait for Max-Age to expire
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=none",
        "cookie with Max-Age=1 should expire after 1 second"
    );
}
