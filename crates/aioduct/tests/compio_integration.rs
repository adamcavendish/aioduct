#![cfg(all(feature = "compio", feature = "tokio"))]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::Client;
use aioduct::runtime::compio_rt::CompioRuntime;

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

fn start_server_tokio() -> SocketAddr {
    start_server_with_tokio(|req| async { hello(req).await })
}

fn start_server_with_tokio<F, Fut>(handler: F) -> SocketAddr
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

#[test]
fn test_compio_redirect_302() {
    let final_addr = start_server_tokio();
    let redirect_addr = start_server_with_tokio(move |_req| {
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
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let resp = client
            .get(&format!("http://{redirect_addr}/"))
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
fn test_compio_timeout_triggers() {
    let addr = start_server_with_tokio(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let result = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .timeout(Duration::from_millis(50))
            .send()
            .await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), aioduct::Error::Timeout),
            "expected Timeout error"
        );
    });
}

#[test]
fn test_compio_custom_header() {
    let addr = start_server_with_tokio(|req| async move {
        let custom = req
            .headers()
            .get("x-custom")
            .map(|v| v.to_str().unwrap_or(""))
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom.to_string()))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .header_str("x-custom", "compio-value")
            .unwrap()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert_eq!(body, "compio-value");
    });
}

fn start_h2_server_tokio<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send + 'static,
{
    use hyper::server::conn::http2 as server_http2;

    #[derive(Clone)]
    struct TokioExec;
    impl<F> hyper::rt::Executor<F> for TokioExec
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            tokio::spawn(fut);
        }
    }

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
                    let _ = server_http2::Builder::new(TokioExec)
                        .serve_connection(io, service_fn(handler))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

#[test]
fn test_compio_h2_prior_knowledge() {
    let addr = start_h2_server_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 compio"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::builder()
            .http2_prior_knowledge()
            .build();

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "h2 compio");
    });
}

#[test]
fn test_compio_h2_multiple_requests() {
    let addr = start_h2_server_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::builder()
            .http2_prior_knowledge()
            .build();
        let url = format!("http://{addr}/");

        let resp1 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        assert_eq!(resp1.text().await.unwrap(), "h2 ok");

        let resp2 = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        assert_eq!(resp2.text().await.unwrap(), "h2 ok");
    });
}

#[test]
fn test_compio_large_body() {
    let addr = start_server_with_tokio(|req| async move {
        let body = req.collect().await.unwrap().to_bytes();
        let len = body.len();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let payload = "x".repeat(1024 * 1024);
        let resp = client
            .post(&format!("http://{addr}/"))
            .unwrap()
            .body(payload)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "1048576");
    });
}

#[test]
fn test_compio_h2_large_body() {
    let addr = start_h2_server_tokio(|req| async move {
        let body = req.collect().await.unwrap().to_bytes();
        let len = body.len();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::builder()
            .http2_prior_knowledge()
            .build();
        let payload = "x".repeat(1024 * 1024);
        let resp = client
            .post(&format!("http://{addr}/"))
            .unwrap()
            .body(payload)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "1048576");
    });
}

#[test]
fn test_compio_large_response_body() {
    let addr = start_server_with_tokio(|_req| async move {
        let body = "y".repeat(512 * 1024);
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    });

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
        assert_eq!(body.len(), 512 * 1024);
    });
}

#[test]
fn test_compio_connection_pool_reuse_after_body_consumed() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("pool test"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let url = format!("http://{addr}/");

        for _ in 0..5 {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "pool test");
        }
    });
}

#[test]
fn test_compio_head_request() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "1000")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let resp = client
            .request(http::Method::HEAD, &format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-length")
                .unwrap()
                .to_str()
                .unwrap(),
            "1000"
        );
        assert_eq!(resp.text().await.unwrap(), "");
    });
}

#[test]
fn test_compio_multiple_headers_same_name() {
    let addr = start_server_with_tokio(|req| async move {
        let vals: Vec<&str> = req
            .headers()
            .get_all("x-multi")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(vals.join(",")))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = Client::<CompioRuntime>::new();
        let mut headers = http::HeaderMap::new();
        headers.append("x-multi", "a".parse().unwrap());
        headers.append("x-multi", "b".parse().unwrap());

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .headers(headers)
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert_eq!(body, "a,b");
    });
}
