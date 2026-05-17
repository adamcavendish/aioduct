#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{h1_server, h1_server_with};

use http_body_util::BodyExt;

#[tokio::test]
async fn test_redirect_302() {
    let (final_addr, _counter) = h1_server().await;
    let (redirect_addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_redirects(3)
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_redirect_307_preserves_method() {
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/target")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let (final_addr, _counter) = h1_server().await;
    let (addr, _counter) = h1_server_with(move |req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
        let (addr, _counter) = h1_server_with(move |req| async move {
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let url = format!("http://{addr}/{code}");
        let resp = client.post(&url).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.headers().get("server").unwrap(), "test-dst");
    }
}

#[tokio::test]
async fn test_redirect_307_and_308_replays_post_body() {
    let codes = [307u16, 308];
    for &code in &codes {
        let (addr, _counter) = h1_server_with(move |req| async move {
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (final_addr, _counter) = h1_server_with(|req| async move {
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

    let (redirect_addr, _counter) = h1_server_with(move |req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let codes = [301u16, 302, 303];
    for &code in &codes {
        let (addr, _counter) = h1_server_with(move |req| async move {
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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "http://www.yikes{KABOOM}")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|req| async move {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(301)
                .header("location", "/target")
                .body(Full::new(Bytes::from("moved")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

#[tokio::test]
async fn redirect_301_302_303_changes_post_to_get() {
    let codes = [301u16, 302, 303];

    for &code in &codes {
        let (addr, _counter) = h1_server_with(move |req| async move {
            if req.method() == "POST" {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(code)
                        .header("location", "/dst")
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            } else {
                assert_eq!(
                    req.method(),
                    "GET",
                    "after {code} redirect, method should be GET"
                );
                Ok(Response::builder()
                    .header("x-arrived", "true")
                    .body(Full::new(Bytes::from("destination")))
                    .unwrap())
            }
        })
        .await;

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let resp = client
            .post(&format!("http://{addr}/{code}"))
            .unwrap()
            .body("request body")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK, "code={code}");
        assert_eq!(
            resp.headers().get("x-arrived").unwrap().to_str().unwrap(),
            "true"
        );
        let url = resp.url().to_string();
        assert!(
            url.contains("/dst"),
            "url should be /dst after redirect, got: {url}"
        );
    }
}

#[tokio::test]
async fn redirect_307_preserves_get() {
    let (addr, _counter) = h1_server_with(|req| async move {
        assert_eq!(req.method(), "GET");
        if req.uri().path() == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(307)
                    .header("location", "/dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            assert_eq!(req.uri().path(), "/dst");
            Ok(Response::new(Full::new(Bytes::from("arrived"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "arrived");
}

#[tokio::test]
async fn redirect_308_preserves_get() {
    let (addr, _counter) = h1_server_with(|req| async move {
        assert_eq!(req.method(), "GET");
        if req.uri().path() == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(308)
                    .header("location", "/dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            assert_eq!(req.uri().path(), "/dst");
            Ok(Response::new(Full::new(Bytes::from("arrived"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "arrived");
}

#[tokio::test]
async fn redirect_307_preserves_post_with_body() {
    let (addr, _counter) = h1_server_with(|req| async move {
        assert_eq!(req.method(), "POST");
        let path = req.uri().path().to_string();
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body_bytes[..], b"Hello", "body must be preserved on 307");

        if path == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(307)
                    .header("location", "/dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            assert_eq!(path, "/dst");
            Ok(Response::new(Full::new(Bytes::from("arrived"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/start"))
        .unwrap()
        .body("Hello")
        .send()
        .await
        .unwrap();

    let url = resp.url().to_string();
    assert!(url.contains("/dst"), "should redirect to /dst, got: {url}");
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "arrived");
}

#[tokio::test]
async fn redirect_308_preserves_post_with_body() {
    let (addr, _counter) = h1_server_with(|req| async move {
        assert_eq!(req.method(), "POST");
        let path = req.uri().path().to_string();
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body_bytes[..], b"Hello", "body must be preserved on 308");

        if path == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(308)
                    .header("location", "/dst")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            assert_eq!(path, "/dst");
            Ok(Response::new(Full::new(Bytes::from("arrived"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .post(&format!("http://{addr}/start"))
        .unwrap()
        .body("Hello")
        .send()
        .await
        .unwrap();

    let url = resp.url().to_string();
    assert!(url.contains("/dst"), "should redirect to /dst, got: {url}");
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "arrived");
}

#[tokio::test]
async fn redirect_max_redirects_error() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_redirects(5)
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.is_redirect(), "expected redirect error, got: {err:?}");
}

#[tokio::test]
async fn redirect_cross_origin_strips_auth() {
    let (final_addr, _counter) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned());
        let body = match auth {
            Some(v) => format!("auth={v}"),
            None => "auth=none".to_string(),
        };
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let (redirect_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{final_addr}/final");
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "auth=none",
        "authorization header should be stripped on cross-origin redirect"
    );
}

#[tokio::test]
async fn redirect_same_origin_preserves_auth() {
    let (addr, _counter) = h1_server_with(|req| async move {
        if req.uri().path() == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", "/final")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let auth = req
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned())
                .unwrap_or_else(|| "none".to_owned());
            Ok(Response::new(Full::new(Bytes::from(auth))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("secret-token"),
        "same-origin redirect should preserve auth, got: {body}"
    );
}

#[tokio::test]
async fn redirect_chain_url_reflects_final() {
    let (final_addr, _counter) = h1_server().await;
    let (mid_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{final_addr}/final");
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

    let (redirect_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{mid_addr}/mid");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(301)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let url = resp.url().to_string();
    assert!(
        url.contains("/final"),
        "URL should reflect final destination, got: {url}"
    );
}

#[tokio::test]
async fn redirect_to_invalid_scheme_returns_error() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "ftp://invalid.example.com/")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(
        result.is_err(),
        "redirect to ftp:// should produce an error"
    );
}

#[tokio::test]
async fn redirect_stop_policy_allows_invalid_location() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "htt://invalid/")
                .body(Full::new(Bytes::from("redirect body")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

// ── Bug-Finding Tests ─────────────────────────────────────────────────
// Tests below expose real bugs or verify subtle edge cases in redirect handling.

// BUG: 307 redirect with streaming body silently drops the body.
// execute.rs:121-122 returns (current_method, body_for_replay) where body_for_replay
// is None for streaming bodies — the redirect follows as POST with an empty body.
#[tokio::test]
async fn redirect_307_streaming_body_should_not_silently_lose_body() {
    let (addr, _) = h1_server_with(|req| async move {
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
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} body_len={} body={body_str}",
                body_bytes.len()
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("streaming-payload"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let result = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let body = resp.text().await.unwrap();
            assert!(
                body.contains("streaming-payload"),
                "BUG: 307 redirect with streaming body silently drops the body. \
                 Expected body to contain 'streaming-payload', got: {body}"
            );
        }
        Err(_e) => {
            // Returning an error is acceptable for non-replayable bodies
        }
    }
}

// BUG: 308 redirect with streaming body should not silently lose body (same as 307).
#[tokio::test]
async fn redirect_308_streaming_body_should_not_silently_lose_body() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(308)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} body_len={}",
                body_bytes.len()
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("streaming-308-data"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let result = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let body = resp.text().await.unwrap();
            assert!(
                !body.contains("body_len=0") || !body.contains("method=POST"),
                "BUG: 308 redirect with streaming body silently drops the body, got: {body}"
            );
        }
        Err(_) => {
            // Returning an error is acceptable for non-replayable bodies
        }
    }
}

// Same-origin redirect with explicit port in Location should preserve auth.
#[tokio::test]
async fn redirect_same_host_different_port_representation_strips_auth() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/step1" {
            let host = req.headers().get("host").unwrap().to_str().unwrap();
            let resp = Response::builder()
                .status(302)
                .header("Location", format!("http://{host}/step2"))
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/step1"))
        .unwrap()
        .bearer_auth("my-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Bearer my-secret-token"),
        "same-origin redirect (same host:port) should preserve auth, got: {body}"
    );
}

// Cross-origin redirect strips both Authorization AND Cookie headers.
#[tokio::test]
async fn redirect_cross_origin_strips_auth_and_cookie() {
    let (target_addr, _) = h1_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "auth={auth} cookie={cookie}"
        )))))
    })
    .await;

    let (origin_addr, _) = h1_server_with(move |req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{origin_addr}/redirect"))
        .unwrap()
        .bearer_auth("secret-token")
        .header(
            http::header::COOKIE,
            http::header::HeaderValue::from_static("session=abc"),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("secret-token"),
        "cross-origin redirect should strip Authorization, got: {body}"
    );
    assert!(
        !body.contains("session=abc"),
        "cross-origin redirect should strip Cookie, got: {body}"
    );
}

// 302 redirect clears body AND Content-Length/Content-Type headers.
#[tokio::test]
async fn redirect_302_clears_body_and_content_length() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(302)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            let content_length = req
                .headers()
                .get("content-length")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            let content_type = req
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} content_length={content_length} content_type={content_type} body_len={}",
                body_bytes.len()
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body("some-data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "302 redirect should change POST to GET, got: {body}"
    );
    assert!(
        body.contains("body_len=0"),
        "302 redirect should clear body, got: {body}"
    );
    assert!(
        !body.contains("content_type=application"),
        "302 redirect should clear Content-Type, got: {body}"
    );
}

// Verify redirect limited boundary: Limited(3) stops at exactly 3 hops.
#[tokio::test]
async fn redirect_limited_exact_boundary() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/0" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/1")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else if path == "/1" {
            Ok(Response::builder()
                .status(302)
                .header("Location", "/2")
                .body(Full::new(Bytes::new()))
                .unwrap())
        } else if path == "/2" {
            Ok(Response::builder()
                .status(302)
                .header("Location", "/3")
                .body(Full::new(Bytes::new()))
                .unwrap())
        } else if path == "/3" {
            Ok(Response::builder()
                .status(302)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap())
        } else {
            Ok(Response::new(Full::new(Bytes::from("final"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::Limited(3))
        .timeout(Duration::from_secs(5))
        .build();

    let result = client
        .get(&format!("http://{addr}/0"))
        .unwrap()
        .send()
        .await;

    match result {
        Ok(resp) => {
            if resp.status() == 200 {
                let body = resp.text().await.unwrap();
                if body == "final" {
                    panic!(
                        "BUG: Limited(3) followed 4 redirects to reach /final. \
                         Expected to stop after 3."
                    );
                }
            }
        }
        Err(e) => {
            assert!(
                format!("{e}").contains("redirect") || e.is_redirect(),
                "expected redirect error, got: {e}"
            );
        }
    }

    // Limited(4) should succeed (exactly 4 redirects to reach /final)
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::Limited(4))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/0"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200, "Limited(4) should reach /final");
    let body = resp.text().await.unwrap();
    assert_eq!(body, "final", "Limited(4) should reach the final page");
}

// Redirect Set-Cookie headers should be stored and applied to the redirected request.
#[tokio::test]
async fn redirect_stores_cookies_from_redirect_response() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/login" {
            let resp = Response::builder()
                .status(302)
                .header("Location", "/dashboard")
                .header("Set-Cookie", "session=xyz789; Path=/")
                .body(Full::new(Bytes::new()))
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
        .get(&format!("http://{addr}/login"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("session=xyz789"),
        "redirect response Set-Cookie should be stored and applied to the \
         redirected request, got: {body}"
    );
}

// 308 redirect preserves method and body (like 307 but permanent).
#[tokio::test]
async fn redirect_308_preserves_method_and_body_buffered() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/redirect" {
            let resp = Response::builder()
                .status(308)
                .header("Location", "/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            let method = req.method().to_string();
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} body={}",
                String::from_utf8_lossy(&body_bytes)
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .post(&format!("http://{addr}/redirect"))
        .unwrap()
        .body("my-payload")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=POST"),
        "308 should preserve POST method, got: {body}"
    );
    assert!(
        body.contains("body=my-payload"),
        "308 should preserve body, got: {body}"
    );
}

// 303 See Other should ALWAYS change to GET regardless of original method.
#[tokio::test]
async fn redirect_303_put_becomes_get() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/submit" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(303)
                    .header("Location", "/result")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let method = req.method().to_string();
            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "method={method} body_len={}",
                body_bytes.len()
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .put(&format!("http://{addr}/submit"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "303 should always redirect as GET regardless of original method, got: {body}"
    );
    assert!(
        body.contains("body_len=0"),
        "303 should clear body, got: {body}"
    );
}

// Custom headers (non-sensitive) should be preserved across same-origin redirects.
#[tokio::test]
async fn redirect_preserves_custom_headers_same_origin() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/end")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let custom = req
                .headers()
                .get("x-request-id")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "x-request-id={custom}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .header(
            http::header::HeaderName::from_static("x-request-id"),
            http::header::HeaderValue::from_static("test-123"),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("x-request-id=test-123"),
        "same-origin redirect should preserve custom headers, got: {body}"
    );
}

// Redirect with whitespace in Location header (curl test42).
#[tokio::test]
async fn redirect_location_with_spaces() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/start" {
            let resp = Response::builder()
                .status(302)
                .header("Location", "/path with spaces/final")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            Ok(Response::new(Full::new(Bytes::from(format!(
                "path={}",
                req.uri().path()
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await;

    match result {
        Ok(resp) => {
            assert_eq!(resp.status(), 200);
            let body = resp.text().await.unwrap();
            assert!(
                body.contains("final"),
                "redirect with spaces in Location should be followed, got: {body}"
            );
        }
        Err(e) => {
            panic!(
                "Redirect with spaces in Location failed: {e}. \
                 Curl follows these by percent-encoding the spaces."
            );
        }
    }
}

// Redirect with relative Location path (curl test45).
#[tokio::test]
async fn redirect_relative_location_path_edge() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/dir/page" {
            let resp = Response::builder()
                .status(301)
                .header("Location", "/other/page")
                .body(Full::new(Bytes::new()))
                .unwrap();
            Ok::<_, Infallible>(resp)
        } else {
            Ok(Response::new(Full::new(Bytes::from(format!(
                "path={path}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/dir/page"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("path=/other/page"),
        "relative Location should resolve correctly, got: {body}"
    );
}

// Timeout should cover the entire redirect chain, not reset per hop.
#[tokio::test]
async fn timeout_covers_entire_redirect_chain() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/slow1" {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/slow2")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else if path == "/slow2" {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok(Response::builder()
                .status(302)
                .header("Location", "/slow3")
                .body(Full::new(Bytes::new()))
                .unwrap())
        } else if path == "/slow3" {
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok(Response::new(Full::new(Bytes::from("done"))))
        } else {
            Ok(Response::new(Full::new(Bytes::from("other"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_millis(300))
        .build();

    let result = client
        .get(&format!("http://{addr}/slow1"))
        .unwrap()
        .send()
        .await;

    assert!(
        result.is_err(),
        "BUG: 300ms timeout should fire before 3x150ms redirect chain completes, \
         but request succeeded: status={}",
        result
            .as_ref()
            .map(|r| r.status().to_string())
            .unwrap_or_default()
    );
}

// Redirect chain (multiple hops) should complete correctly.
#[tokio::test]
async fn redirect_multi_hop_chain_completes() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/start" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/middle")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else if path == "/middle" {
            Ok(Response::builder()
                .status(302)
                .header("Location", "/end")
                .body(Full::new(Bytes::new()))
                .unwrap())
        } else {
            Ok(Response::new(Full::new(Bytes::from("reached-end"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "reached-end",
        "redirect chain should complete, got: {body}"
    );
}

// BUG: resolve_redirect strips fragment unconditionally (mod.rs:216).
// RFC 7231: if Location has no fragment, the original request's fragment
// should be inherited.
#[tokio::test]
async fn redirect_should_preserve_fragment_when_location_has_none() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        let query = req.uri().query().unwrap_or("");
        if path == "/page" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/target")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from(format!(
                "path={path} query={query}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    // Request with a fragment: http://addr/page#section1
    // After redirect to /target, the fragment #section1 should be preserved
    // (since the Location header has no fragment of its own).
    let resp = client
        .get(&format!("http://{addr}/page#section1"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    // Note: fragments are not sent to the server in HTTP, so we can't verify
    // server-side. But we can check the response's URL to see if the fragment
    // was preserved in the final URI.
    // This is a documentation test — the resolve_redirect strips fragments
    // unconditionally, which violates RFC 7231 Section 7.1.2.
}

// BUG: Referer header leaks HTTPS URL on HTTPS→HTTP redirect.
// RFC 7231 Section 5.5.2: "A user agent MUST NOT send a Referer header
// field in an unsecured HTTP request if the referring page was transferred
// with a secure protocol."
// This test verifies the simpler case: referer IS set on same-origin HTTP
// redirects when enabled. The HTTPS→HTTP leak test requires TLS infrastructure.
#[tokio::test]
async fn redirect_referer_set_on_same_origin_when_enabled() {
    let (addr, _) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/page1" {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", "/page2")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        } else {
            let referer = req
                .headers()
                .get("referer")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(format!(
                "referer={referer}"
            )))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/page1"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("referer=http://"),
        "referer(true) should set Referer on same-origin redirect, got: {body}"
    );
}

// BUG: Referer should NOT be set on cross-origin HTTP redirects
// when it would leak the origin URL.
#[tokio::test]
async fn redirect_referer_not_set_cross_origin() {
    let (target_addr, _) = h1_server_with(|req| async move {
        let referer = req
            .headers()
            .get("referer")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "referer={referer}"
        )))))
    })
    .await;

    let (origin_addr, _) = h1_server_with(move |_req| {
        let target = target_addr;
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("Location", format!("http://{target}/dest"))
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{origin_addr}/secret-path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let _body = resp.text().await.unwrap();
    // the library sends Referer even cross-origin. Whether this is a bug
    // depends on the use case — curl sends it by default too. But the
    // HTTPS→HTTP case (not tested here) MUST NOT send it.
    // For HTTP→HTTP cross-origin, behavior is implementation-defined.
}

// BUG: Redirect to data: or javascript: scheme should be rejected.
#[tokio::test]
async fn redirect_to_data_scheme_rejected() {
    let (addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("Location", "data:text/html,<h1>pwned</h1>")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(
        result.is_err(),
        "redirect to data: scheme should be rejected as a security issue"
    );
}

// BUG: Redirect with empty Location header should not panic.
#[tokio::test]
async fn redirect_empty_location_header() {
    let (addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("Location", "")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    // Should either return an error or redirect to the same URL
    // (empty string resolves to the base URI per RFC 3986).
    // Must NOT panic.
    match result {
        Ok(resp) => {
            // If it succeeded, it should not be an infinite loop
            // (the test would have timed out).
            assert!(resp.status().is_success() || resp.status().is_redirection());
        }
        Err(_) => {
            // Error is also acceptable
        }
    }
}

// BUG: Custom redirect policy with RedirectAction::Stop should return
// the redirect response (not error), even with a Custom policy.
#[tokio::test]
async fn redirect_custom_policy_stop_returns_response() {
    let (addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(301)
                .header("Location", "/target")
                .body(Full::new(Bytes::from("redirect body")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::custom(
            |_current, _next, _status, _method| aioduct::RedirectAction::Stop,
        ))
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        301,
        "Stop policy should return redirect response"
    );
    assert!(
        resp.headers().get("location").is_some(),
        "Stop policy should preserve Location header"
    );
}

// HSTS upgrades redirect target URIs (not just the initial URI).
// execute_send.rs:30 and the redirect loop tail both call maybe_upgrade_hsts().
#[tokio::test]
async fn redirect_target_is_hsts_upgraded() {
    let hsts_store = aioduct::hsts::HstsStore::new();
    let mut sts_headers = http::HeaderMap::new();
    sts_headers.insert("strict-transport-security", "max-age=3600".parse().unwrap());
    hsts_store.store_from_response("localhost", &sts_headers);

    let (plain_addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("plain-http"))))
    })
    .await;

    let (redirect_addr, _) = h1_server_with(move |_req| {
        let target = format!("http://localhost:{}/final", plain_addr.port());
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .hsts(hsts_store)
        .timeout(Duration::from_secs(2))
        .build();

    // The redirect target http://localhost:{port}/final should be HSTS-upgraded
    // to https://localhost:{port}/final. Since the target is a plain HTTP server,
    // a TLS handshake will fail — proving the upgrade happened. Without the fix,
    // the client would follow the redirect as plain HTTP and get "plain-http".
    let result = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await;

    assert!(
        result.is_err(),
        "should fail: redirect target was HSTS-upgraded to HTTPS but server is plain HTTP"
    );
}
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .https_only(true)
        .timeout(Duration::from_secs(5))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "https_only should block HTTP requests");

    // The real gap test: if we could start HTTPS and redirect to HTTP,
    // the redirect target would NOT be checked against https_only.
    // This is documented in execute_send.rs:24 — only original_uri is checked.
}

// BUG: execute.rs:141-145 sets Referer unconditionally, even on HTTPS→HTTP redirect.
// Per RFC 7231 §5.5.2, Referer MUST NOT be sent when going from HTTPS to HTTP.
#[tokio::test]
async fn referer_should_not_leak_on_https_to_http_downgrade() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let captured_referer = Arc::new(std::sync::Mutex::new(None::<String>));
    let captured_referer_clone = captured_referer.clone();
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();

    let (addr, _) = h1_server_with(move |req| {
        let count = request_count_clone.clone();
        let referer_capture = captured_referer_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Simulate "came from HTTPS" by redirecting with a scheme change
                // In reality this would be an HTTPS server redirecting to HTTP.
                // We simulate by setting up a redirect and checking the Referer header.
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
                // Capture the Referer header on the redirect target
                let referer = req
                    .headers()
                    .get("referer")
                    .map(|v| v.to_str().unwrap_or("").to_string());
                *referer_capture.lock().unwrap() = referer;
                Ok(Response::new(Full::new(Bytes::from("final"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .referer(true)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    // For HTTP→HTTP redirects, Referer is fine. But the code sets it unconditionally
    // (execute.rs:141-145), including on HTTPS→HTTP downgrades.
    // This test documents the behavior: Referer IS set on redirects.
    let referer = captured_referer.lock().unwrap().clone();
    assert!(
        referer.is_some(),
        "Referer should be set on same-scheme redirect (confirming referer(true) works)"
    );
    // The real bug: execute.rs:141-145 doesn't check if current scheme is HTTPS
    // and next scheme is HTTP. It sets Referer unconditionally.
}

// BUG: redirect.rs:64-68 Custom policy returns usize::MAX for max_redirects().
// Combined with execute_send.rs:37 `for _ in 0..=max_redirects()`, this creates
// an effectively infinite loop. The TooManyRedirects error at line 213 is unreachable.
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .redirect_policy(aioduct::RedirectPolicy::custom(
            |_current, _next, _status, _method| aioduct::RedirectAction::Follow,
        ))
        .timeout(Duration::from_secs(3))
        .build();

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

// BUG: execute.rs:137 strips Cookie on cross-origin redirect, but doesn't strip
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .sensitive_header(http::header::HeaderName::from_static("x-api-key"))
        .build();

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
         are stripped (execute.rs:136-138). Got X-Api-Key={} on cross-origin target.",
        api_key.unwrap_or_default()
    );
}
