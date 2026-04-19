#![cfg(feature = "blocking")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::blocking::Client;

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
    let client = Client::new();
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
    let client = Client::new();
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

    let client = Client::new();
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
    let client = Client::builder()
        .timeout(Duration::from_millis(100))
        .build();
    let result = client.get(&format!("http://{addr}/")).unwrap().send();

    assert!(result.is_err());
}

#[test]
fn blocking_head_request() {
    let addr = start_server_with(hello);
    let client = Client::new();
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
    let client = Client::new();
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
    let client = Client::new();
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
    let client = Client::new();
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
    let client = Client::new();
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

    let client = Client::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .unwrap();
    let data: serde_json::Value = resp.json().unwrap();
    assert_eq!(data["key"], "value");
}
