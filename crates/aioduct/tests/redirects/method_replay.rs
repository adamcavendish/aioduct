use std::convert::Infallible;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::h1::h1_server_with;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Response;

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

        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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
