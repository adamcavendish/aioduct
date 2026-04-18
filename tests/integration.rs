use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

use aioduct::Client;
use aioduct::runtime::TokioRuntime;

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

async fn start_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(io, service_fn(hello))
                    .await;
            });
        }
    });

    addr
}

#[tokio::test]
async fn test_get_request() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_post_request() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("request body")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn test_connection_reuse() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::new();
    let url = format!("http://{addr}/");

    let resp1 = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp1.status(), http::StatusCode::OK);
    let _ = resp1.text().await.unwrap();

    let resp2 = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp2.status(), http::StatusCode::OK);
    let body = resp2.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_invalid_url() {
    let client = Client::<TokioRuntime>::new();
    assert!(client.get("not a url").is_err());
}

#[tokio::test]
async fn test_missing_scheme() {
    let client = Client::<TokioRuntime>::new();
    // "127.0.0.1/path" is not a valid absolute URI, so get() returns an error
    assert!(client.get("127.0.0.1/path").is_err());
}
