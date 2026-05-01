#![cfg(feature = "smol")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::Client;
use aioduct::runtime::smol_rt::{SmolIo, SmolRuntime};

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

async fn start_server() -> SocketAddr {
    start_server_with(|req| async { hello(req).await }).await
}

async fn start_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    smol::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = SmolIo::new(stream);
            let handler = handler.clone();
            smol::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(io, service_fn(handler))
                    .await;
            })
            .detach();
        }
    })
    .detach();

    addr
}

#[test]
fn test_smol_get_request() {
    smol::block_on(async {
        let addr = start_server().await;
        let client = Client::<SmolRuntime>::new();

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
fn test_smol_post_request() {
    smol::block_on(async {
        let addr = start_server().await;
        let client = Client::<SmolRuntime>::new();

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
fn test_smol_connection_reuse() {
    smol::block_on(async {
        let addr = start_server().await;
        let client = Client::<SmolRuntime>::new();
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
fn test_smol_redirect_302() {
    smol::block_on(async {
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

        let client = Client::<SmolRuntime>::new();
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
fn test_smol_timeout_triggers() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async {
            smol::Timer::after(Duration::from_secs(5)).await;
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
        })
        .await;

        let client = Client::<SmolRuntime>::new();
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
fn test_smol_custom_header() {
    smol::block_on(async {
        let addr = start_server_with(|req| async move {
            let custom = req
                .headers()
                .get("x-custom")
                .map(|v| v.to_str().unwrap_or(""))
                .unwrap_or("missing");
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom.to_string()))))
        })
        .await;

        let client = Client::<SmolRuntime>::new();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .header_str("x-custom", "smol-value")
            .unwrap()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert_eq!(body, "smol-value");
    });
}

async fn start_h2_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send + 'static,
{
    use hyper::server::conn::http2 as server_http2;

    #[derive(Clone)]
    struct SmolExec;
    impl<F> hyper::rt::Executor<F> for SmolExec
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            smol::spawn(fut).detach();
        }
    }

    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    smol::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = SmolIo::new(stream);
            let handler = handler.clone();
            smol::spawn(async move {
                let _ = server_http2::Builder::new(SmolExec)
                    .serve_connection(io, service_fn(handler))
                    .await;
            })
            .detach();
        }
    })
    .detach();

    addr
}

#[test]
fn test_smol_h2_prior_knowledge() {
    smol::block_on(async {
        let addr = start_h2_server_with(|_req| async {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 smol"))))
        })
        .await;

        let client = Client::<SmolRuntime>::builder()
            .http2_prior_knowledge()
            .build();

        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "h2 smol");
    });
}

#[test]
fn test_smol_h2_multiple_requests() {
    smol::block_on(async {
        let addr = start_h2_server_with(|_req| async {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2 ok"))))
        })
        .await;

        let client = Client::<SmolRuntime>::builder()
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
fn test_smol_large_body() {
    smol::block_on(async {
        let addr = start_server_with(|req| async move {
            let body = req.collect().await.unwrap().to_bytes();
            let len = body.len();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
        })
        .await;

        let client = Client::<SmolRuntime>::new();
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
fn test_smol_h2_large_body() {
    smol::block_on(async {
        let addr = start_h2_server_with(|req| async move {
            let body = req.collect().await.unwrap().to_bytes();
            let len = body.len();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{len}")))))
        })
        .await;

        let client = Client::<SmolRuntime>::builder()
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
fn test_smol_large_response_body() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async move {
            let body = "y".repeat(512 * 1024);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
        })
        .await;

        let client = Client::<SmolRuntime>::new();
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
fn test_smol_connection_pool_reuse_after_body_consumed() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("pool test"))))
        })
        .await;

        let client = Client::<SmolRuntime>::new();
        let url = format!("http://{addr}/");

        for _ in 0..5 {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "pool test");
        }
    });
}

#[test]
fn test_smol_head_request() {
    smol::block_on(async {
        let addr = start_server_with(|_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", "1000")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        })
        .await;

        let client = Client::<SmolRuntime>::new();
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
fn test_smol_multiple_headers_same_name() {
    smol::block_on(async {
        let addr = start_server_with(|req| async move {
            let vals: Vec<&str> = req
                .headers()
                .get_all("x-multi")
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect();
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(vals.join(",")))))
        })
        .await;

        let client = Client::<SmolRuntime>::new();
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
