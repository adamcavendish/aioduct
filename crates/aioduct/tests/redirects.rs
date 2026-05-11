#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_redirect_302() {
    let final_addr = start_server().await;
    let redirect_addr = start_server_with(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{redirect_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_redirect_relative() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/redirect" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "/final")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from("final destination"))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/redirect"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "final destination");
}

#[tokio::test]
async fn test_redirect_max_exceeded() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_redirects(3)
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_redirect_307_preserves_method() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/redirect" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(307)
                    .header("location", "/final")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let method = req.method().to_string();
            Ok(Response::new(Full::new(Bytes::from(method))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "POST");
}

#[tokio::test]
async fn test_redirect_303_changes_to_get() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/redirect" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(303)
                    .header("location", "/final")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let method = req.method().to_string();
            Ok(Response::new(Full::new(Bytes::from(method))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "GET");
}
#[tokio::test]
async fn test_redirect_policy_none() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/target")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::none())
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::FOUND);
}

#[tokio::test]
async fn test_redirect_policy_custom() {
    let final_addr = start_server().await;
    let addr = start_server_with(move |req| {
        let target = format!("http://{final_addr}/");
        async move {
            if req.uri().path() == "/allowed" {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", target)
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", "/blocked-target")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            }
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::custom(
            |_current, next, _status, _method| {
                if next.host() == Some("127.0.0.1") {
                    aioduct::RedirectAction::Follow
                } else {
                    aioduct::RedirectAction::Stop
                }
            },
        ))
        .build();

    let resp = client
        .get(&format!("http://{addr}/allowed"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}
#[tokio::test]
async fn test_redirect_301_and_302_and_303_changes_post_to_get() {
    let codes = [301u16, 302, 303];
    for &code in &codes {
        let addr = start_server_with(move |req| async move {
            if req.method() == "POST" {
                assert_eq!(req.uri().path(), &format!("/{code}"));
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(code)
                        .header("location", "/dst")
                        .header("server", "test-redirect")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                assert_eq!(req.method(), "GET");
                Ok(Response::builder()
                    .header("server", "test-dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap())
            }
        })
        .await;

        let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let url = format!("http://{addr}/{code}");
        let resp = client.post(&url).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.headers().get("server").unwrap(), "test-dst");
    }
}

#[tokio::test]
async fn test_redirect_307_and_308_replays_post_body() {
    use http_body_util::BodyExt;

    let codes = [307u16, 308];
    for &code in &codes {
        let addr = start_server_with(move |req| async move {
            assert_eq!(req.method(), "POST");
            let uri = req.uri().path().to_owned();
            let body = req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&body[..], b"Hello");

            if uri == "/dst" {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("server", "test-dst")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                Ok(Response::builder()
                    .status(code)
                    .header("location", "/dst")
                    .header("server", "test-redirect")
                    .body(Full::new(Bytes::new()))
                    .unwrap())
            }
        })
        .await;

        let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let url = format!("http://{addr}/{code}");
        let resp = client
            .post(&url)
            .unwrap()
            .body("Hello")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    }
}

#[tokio::test]
async fn test_redirect_removes_sensitive_headers_cross_origin() {
    let final_addr = start_server_with(|req| async move {
        assert!(
            req.headers().get("cookie").is_none(),
            "cookie should be stripped on cross-origin redirect"
        );
        assert!(
            req.headers().get("authorization").is_none(),
            "authorization should be stripped on cross-origin redirect"
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    let redirect_addr = start_server_with(move |req| {
        let target = format!("http://{final_addr}/end");
        async move {
            assert_eq!(req.headers().get("cookie").unwrap(), "foo=bar");
            assert_eq!(req.headers().get("authorization").unwrap(), "Bearer token");
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{redirect_addr}/sensitive"))
        .unwrap()
        .header(
            http::header::COOKIE,
            http::header::HeaderValue::from_static("foo=bar"),
        )
        .header(
            http::header::AUTHORIZATION,
            http::header::HeaderValue::from_static("Bearer token"),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn test_redirect_301_302_303_strips_content_headers() {
    use http_body_util::BodyExt;

    let codes = [301u16, 302, 303];
    for &code in &codes {
        let addr = start_server_with(move |req| async move {
            if req.method() == "POST" {
                let body = req.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(&body[..], b"Hello");
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(code)
                        .header("location", "/dst")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                assert_eq!(req.method(), "GET");
                assert!(
                    req.headers().get("content-type").is_none(),
                    "content-type should be stripped after {code} POST->GET"
                );
                assert!(
                    req.headers().get("content-length").is_none(),
                    "content-length should be stripped after {code} POST->GET"
                );
                Ok(Response::builder()
                    .header("server", "test-dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap())
            }
        })
        .await;

        let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let url = format!("http://{addr}/{code}");
        let resp = client
            .post(&url)
            .unwrap()
            .body("Hello")
            .header(
                http::header::CONTENT_TYPE,
                http::header::HeaderValue::from_static("text/plain"),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.headers().get("server").unwrap(), "test-dst");
    }
}

#[tokio::test]
async fn test_redirect_invalid_location_stops() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "http://www.yikes{KABOOM}")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let result = client
        .get(&format!("http://{addr}/yikes"))
        .unwrap()
        .send()
        .await;

    assert!(
        result.is_err(),
        "invalid Location URL should cause an error"
    );
}

#[tokio::test]
async fn test_redirect_loop_returns_error() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let result = client
        .get(&format!("http://{addr}/loop"))
        .unwrap()
        .send()
        .await;

    assert!(result.is_err(), "redirect loop should return error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("too many redirects"),
        "error should mention redirect limit, got: {err}"
    );
}

#[tokio::test]
async fn test_redirect_limit_to_1_ported() {
    let addr = start_server_with(|req| async move {
        let i: i32 = req
            .uri()
            .path()
            .rsplit('/')
            .next()
            .unwrap()
            .parse::<i32>()
            .unwrap_or(0);

        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", format!("/redirect/{}", i + 1))
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_redirects(1)
        .build();
    let result = client
        .get(&format!("http://{addr}/redirect/0"))
        .unwrap()
        .send()
        .await;

    assert!(
        result.is_err(),
        "should fail after 1 redirect with max_redirects(1)"
    );
}

#[tokio::test]
async fn test_redirect_302_with_set_cookies() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/302" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "/dst")
                    .header("set-cookie", "key=value")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            assert_eq!(req.uri().path(), "/dst");
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap().to_owned());
            let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
            Ok(Response::new(Full::new(Bytes::from(body))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(aioduct::CookieJar::new())
        .build();

    let resp = client
        .get(&format!("http://{addr}/302"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "cookie=key=value");
}

#[tokio::test]
async fn test_redirect_referer_is_set_when_enabled() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "/dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let referer = req
                .headers()
                .get("referer")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_else(|| "none".into());
            Ok(Response::new(Full::new(Bytes::from(referer))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .referer(true)
        .build();
    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("/start"),
        "referer should contain original URL, got: {body}"
    );
}

#[tokio::test]
async fn test_redirect_referer_not_set_by_default() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "/dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let has_referer = req.headers().get("referer").is_some();
            let body = format!("has_referer={has_referer}");
            Ok(Response::new(Full::new(Bytes::from(body))))
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "has_referer=false");
}
#[tokio::test]
async fn test_304_not_modified_is_not_treated_as_redirect() {
    let addr = start_server_with(|req| async move {
        if req.headers().get("if-none-match").is_some() {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(304)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::builder()
                .status(200)
                .header("etag", "W/\"abc\"")
                .body(Full::new(Bytes::from("hello")))
                .unwrap())
        }
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("if-none-match", "W/\"abc\"")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn test_redirect_stop_returns_redirect_response() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(301)
                .header("location", "/target")
                .body(Full::new(Bytes::from("moved")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::custom(
            |_current, _next, _status, _method| aioduct::RedirectAction::Stop,
        ))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::MOVED_PERMANENTLY);
}
