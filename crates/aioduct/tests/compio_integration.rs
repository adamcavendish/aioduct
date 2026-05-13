#![cfg(all(feature = "compio", feature = "tokio"))]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::HttpEngine;
use aioduct::runtime::compio_rt::{CompioRuntime, TcpConnector};

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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let resp = client
            .get_local(&format!("http://{addr}/"))
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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let resp = client
            .post_local(&format!("http://{addr}/"))
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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let url = format!("http://{addr}/");

        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let resp = client
            .get_local(&format!("http://{redirect_addr}/"))
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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let result = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .timeout(Duration::from_millis(50))
            .send()
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().is_timeout(), "expected Timeout error");
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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let resp = client
            .get_local(&format!("http://{addr}/"))
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

#[test]
fn test_compio_h2_prior_knowledge() {
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

    let addr = {
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
                        let _ = server_http2::Builder::new(TokioExec)
                            .serve_connection(
                                io,
                                service_fn(|_req| async {
                                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                        "h2 compio",
                                    ))))
                                }),
                            )
                            .await;
                    });
                }
            });
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngine::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .http2_prior_knowledge()
            .build_local();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "h2 compio");
    });
}

#[test]
fn test_compio_large_body() {
    let addr = start_server_with_tokio(|req| async move {
        use http_body_util::BodyExt;
        let body = req.collect().await.unwrap().to_bytes();
        let len = body.len();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let payload = "x".repeat(1024 * 1024);
        let resp = client
            .post_local(&format!("http://{addr}/"))
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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let resp = client
            .get_local(&format!("http://{addr}/"))
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
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let resp = client
            .request_local(http::Method::HEAD, &format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.content_length(), Some(1000));
    });
}

#[test]
fn test_compio_default_headers() {
    let addr = start_server_with_tokio(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngine::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .user_agent("compio-test/1.0")
            .build_local();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "compio-test/1.0");
    });
}

#[test]
fn test_compio_pool_reuse_after_body_consumed() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("pool test"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let url = format!("http://{addr}/");

        for _ in 0..5 {
            let resp = client.get_local(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "pool test");
        }
    });
}

#[test]
fn test_compio_bearer_auth() {
    let addr = start_server_with_tokio(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .bearer_auth("my-token")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "Bearer my-token");
    });
}

#[test]
fn test_compio_http_client_trait() {
    use aioduct::HttpClientLocal;
    use aioduct::traits::{HttpClient, RequestBuilderExt};

    let addr = start_server_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let engine = HttpEngine::<CompioRuntime, TcpConnector>::new_local(TcpConnector);
        let client = HttpClientLocal::new(engine);

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });
}
