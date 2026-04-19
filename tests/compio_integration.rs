#![cfg(all(feature = "compio", feature = "tokio"))]

use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::Client;
use aioduct::runtime::compio_rt::CompioRuntime;

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

fn start_server_tokio() -> SocketAddr {
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
                tokio::spawn(async move {
                    let _ = server_http1::Builder::new()
                        .serve_connection(io, service_fn(hello))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

#[test]
fn test_compio_get_request() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_post_request() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let resp = client
            .post(&format!("http://{addr}/"))
            .unwrap()
            .body("request body")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_connection_reuse() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let url = format!("http://{addr}/");

        let resp1 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        let resp2 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let body = resp2.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}
