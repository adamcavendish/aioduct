#![cfg(all(feature = "blocking", feature = "tokio"))]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::BlockingTokioClient;
use aioduct::TokioClient;
use aioduct::runtime::tokio_rt::TcpConnector;

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello blocking"))))
}

async fn echo_body(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    use http_body_util::BodyExt;
    let body = req.collect().await.unwrap().to_bytes();
    Ok(Response::new(Full::new(body)))
}

async fn slow(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(Response::new(Full::new(Bytes::from("slow"))))
}

fn start_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = server_http1::Builder::new()
                        .serve_connection(io, service_fn(handler))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

#[test]
fn blocking_get() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().unwrap();
    assert_eq!(body, "hello blocking");
}

#[test]
fn blocking_post_with_body() {
    let addr = start_server_with(echo_body);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("request body")
        .send()
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().unwrap();
    assert_eq!(body, "request body");
}

#[test]
fn blocking_custom_header() {
    let addr = start_server_with(|req: Request<hyper::body::Incoming>| async move {
        let val = req
            .headers()
            .get("x-custom")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    });

    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::HeaderName::from_static("x-custom"),
            http::header::HeaderValue::from_static("test-value"),
        )
        .send()
        .unwrap();

    assert_eq!(resp.text().unwrap(), "test-value");
}

#[test]
fn blocking_timeout() {
    let addr = start_server_with(slow);
    let client = BlockingTokioClient::new(
        TokioClient::builder(TcpConnector)
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap(),
    );
    let result = client.get(&format!("http://{addr}/")).unwrap().send();
    assert!(result.is_err());
}

#[test]
fn blocking_head_request() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .head(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[test]
fn blocking_put_request() {
    let addr = start_server_with(echo_body);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .put(&format!("http://{addr}/"))
        .unwrap()
        .body("put data")
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "put data");
}

#[test]
fn blocking_error_for_status() {
    let addr = start_server_with(|_req: Request<hyper::body::Incoming>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert!(resp.error_for_status().is_err());
}

#[test]
fn blocking_connection_reuse() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let url = format!("http://{addr}/");

    let resp1 = client.get(&url).unwrap().send().unwrap();
    assert_eq!(resp1.status(), http::StatusCode::OK);
    let _ = resp1.bytes().unwrap();

    let resp2 = client.get(&url).unwrap().send().unwrap();
    assert_eq!(resp2.status(), http::StatusCode::OK);
    assert_eq!(resp2.text().unwrap(), "hello blocking");
}

#[test]
fn blocking_content_length() {
    let addr = start_server_with(|_req: Request<hyper::body::Incoming>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("Content-Length", "5")
                .body(Full::new(Bytes::from("12345")))
                .unwrap(),
        )
    });
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(resp.content_length(), Some(5));
}

#[cfg(feature = "json")]
#[test]
fn blocking_json() {
    let addr = start_server_with(|_req: Request<hyper::body::Incoming>| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(r#"{"key":"value"}"#))))
    });

    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    let data: serde_json::Value = resp.json().unwrap();
    assert_eq!(data["key"], "value");
}

#[test]
fn blocking_default_headers() {
    let addr = start_server_with(|req: Request<hyper::body::Incoming>| async move {
        let val = req
            .headers()
            .get("x-default")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    });

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::HeaderName::from_static("x-default"),
        http::header::HeaderValue::from_static("default-val"),
    );
    let client = BlockingTokioClient::new(
        TokioClient::builder(TcpConnector)
            .default_headers(headers)
            .build()
            .unwrap(),
    );
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "default-val");
}

#[test]
fn blocking_override_default_headers() {
    let addr = start_server_with(|req: Request<hyper::body::Incoming>| async move {
        let val = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    });

    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::header::HeaderValue::from_static("default-token"),
    );
    let client = BlockingTokioClient::new(
        TokioClient::builder(TcpConnector)
            .default_headers(headers)
            .build()
            .unwrap(),
    );
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::AUTHORIZATION,
            http::header::HeaderValue::from_static("override-token"),
        )
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "override-token");
}

#[test]
fn blocking_error_for_status_5xx() {
    let addr = start_server_with(|_req: Request<hyper::body::Incoming>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(500)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    let err = resp.error_for_status().unwrap_err();
    assert!(err.is_status());
}

#[test]
fn blocking_get_no_content_length() {
    let addr = start_server_with(|req: Request<hyper::body::Incoming>| async move {
        assert!(
            req.headers().get("content-length").is_none(),
            "GET should not set content-length"
        );
        assert!(
            req.headers().get("transfer-encoding").is_none(),
            "GET should not set transfer-encoding"
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    });

    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[test]
fn blocking_https_only_rejects_http() {
    let client = BlockingTokioClient::new(
        TokioClient::builder(TcpConnector)
            .https_only(true)
            .build()
            .unwrap(),
    );
    let result = client.get("http://example.com/").unwrap().send();
    assert!(result.is_err());
}

#[test]
fn blocking_remote_addr() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    let remote = resp.remote_addr();
    assert!(remote.is_some());
    assert_eq!(remote.unwrap().port(), addr.port());
}

#[test]
fn blocking_patch_request() {
    let addr = start_server_with(echo_body);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .patch(&format!("http://{addr}/"))
        .unwrap()
        .body("patch data")
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "patch data");
}

#[test]
fn blocking_delete_request() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .delete(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[test]
fn blocking_custom_method() {
    let addr = start_server_with(|req: Request<hyper::body::Incoming>| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    });

    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .request(http::Method::OPTIONS, &format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "OPTIONS");
}

#[test]
fn blocking_bearer_auth() {
    let addr = start_server_with(|req: Request<hyper::body::Incoming>| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    });

    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .bearer_auth("test-token")
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "Bearer test-token");
}

#[test]
fn blocking_bulk_headers() {
    let addr = start_server_with(|req: Request<hyper::body::Incoming>| async move {
        let h1 = req
            .headers()
            .get("x-one")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let h2 = req
            .headers()
            .get("x-two")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{h1},{h2}")))))
    });

    let mut headers = http::HeaderMap::new();
    headers.insert("x-one", "1".parse().unwrap());
    headers.insert("x-two", "2".parse().unwrap());

    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .headers(headers)
        .send()
        .unwrap();
    assert_eq!(resp.text().unwrap(), "1,2");
}

#[test]
fn blocking_error_for_status_ref_4xx() {
    let addr = start_server_with(|_req: Request<hyper::body::Incoming>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(403)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert!(resp.error_for_status_ref().is_err());
    assert_eq!(resp.status(), http::StatusCode::FORBIDDEN);
}

#[test]
fn blocking_error_for_status_ref_ok() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert!(resp.error_for_status_ref().is_ok());
}

#[test]
fn blocking_url() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/path"))
        .unwrap()
        .send()
        .unwrap();
    assert!(resp.url().to_string().contains("/path"));
}

#[test]
fn blocking_version() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(resp.version(), http::Version::HTTP_11);
}

#[test]
fn blocking_tls_info_none_for_http() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert!(resp.tls_info().is_none());
}

#[test]
fn blocking_bytes_method() {
    let addr = start_server_with(|_req: Request<hyper::body::Incoming>| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("raw bytes data"))))
    });
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    let bytes = resp.bytes().unwrap();
    assert_eq!(bytes, Bytes::from("raw bytes data"));
}

#[test]
fn blocking_response_debug_format() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    let debug_str = format!("{resp:?}");
    assert!(
        debug_str.contains("BlockingResponse"),
        "Debug format should contain BlockingResponse, got: {debug_str}"
    );
    assert!(
        debug_str.contains("200"),
        "Debug format should contain status code, got: {debug_str}"
    );
}

#[test]
fn blocking_error_for_status_ok_passes() {
    let addr = start_server_with(hello);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    let resp = resp.error_for_status().unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[test]
fn blocking_headers_accessor() {
    let addr = start_server_with(|_req: Request<hyper::body::Incoming>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("x-test", "test-val")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    });
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    assert_eq!(
        resp.headers().get("x-test").unwrap().to_str().unwrap(),
        "test-val"
    );
}

#[test]
fn blocking_request_timeout_per_request() {
    let addr = start_server_with(slow);
    let client = BlockingTokioClient::new(TokioClient::new(TcpConnector));
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(50))
        .send();
    assert!(result.is_err(), "per-request timeout should fire");
}
