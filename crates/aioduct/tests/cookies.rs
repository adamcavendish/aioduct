#![cfg(feature = "tokio")]

mod common;
use common::*;

use aioduct::SameSite;

#[tokio::test]
async fn test_cookie_jar_stores_and_sends() {
    let addr = start_server_with(|req| async move {
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
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let addr = start_server_with(|req| async move {
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
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let addr = start_server_with(|req| async move {
        let has_cookie = req.headers().contains_key("cookie");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "has_cookie={has_cookie}"
        )))))
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
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

    let addr1 = start_server_with(|req| async move {
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

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let addr = start_server_with(move |req| {
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
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let addr = start_server_with(move |req| {
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
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let addr = start_server_with(|req| async move {
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
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let addr = start_server_with(|req| async move {
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
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header(
                    "set-cookie",
                    "key=val; Domain=example.com; Path=/api; Secure; HttpOnly; SameSite=Strict",
                )
                .header("set-cookie", "lax_cookie=lax; SameSite=Lax")
                .header("set-cookie", "plain=text")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    assert_eq!(key_cookie.domain(), Some("example.com"));
    assert_eq!(key_cookie.path(), Some("/api"));
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
    assert_eq!(plain_cookie.path(), None);
    assert!(!plain_cookie.secure());
    assert!(!plain_cookie.http_only());
    assert_eq!(plain_cookie.same_site(), None);
}
