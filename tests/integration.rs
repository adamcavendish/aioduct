use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

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

async fn echo_headers(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let host = req
        .headers()
        .get("host")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("missing");
    let path = req.uri().path().to_string();
    let body = format!("host={host}\npath={path}");
    Ok(Response::new(Full::new(Bytes::from(body))))
}

async fn start_server() -> SocketAddr {
    start_server_with(|req| async { hello(req).await }).await
}

async fn start_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
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
async fn test_host_header_and_path() {
    let addr = start_server_with(echo_headers).await;
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get(&format!("http://{addr}/some/path?key=value"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(&format!("host={addr}")),
        "expected Host header to be set, got: {body}"
    );
    assert!(
        body.contains("path=/some/path"),
        "expected path-only URI, got: {body}"
    );
}

#[tokio::test]
async fn test_custom_header() {
    let addr = start_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-custom")
            .map(|v| v.to_str().unwrap_or(""))
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom.to_string()))))
    })
    .await;
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("x-custom", "test-value")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "test-value");
}

#[tokio::test]
async fn test_invalid_url() {
    let client = Client::<TokioRuntime>::new();
    assert!(client.get("not a url").is_err());
}

#[tokio::test]
async fn test_missing_scheme() {
    let client = Client::<TokioRuntime>::new();
    assert!(client.get("127.0.0.1/path").is_err());
}

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

    let client = Client::<TokioRuntime>::new();
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

    let client = Client::<TokioRuntime>::new();
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

    let client = Client::<TokioRuntime>::builder().max_redirects(3).build();

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

    let client = Client::<TokioRuntime>::new();
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

    let client = Client::<TokioRuntime>::new();
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
async fn test_request_timeout_triggers() {
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(50))
        .send()
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, aioduct::Error::Timeout),
        "expected Timeout error, got: {err:?}"
    );
}

#[tokio::test]
async fn test_request_timeout_completes_in_time() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_client_default_timeout_triggers() {
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .timeout(Duration::from_millis(50))
        .build();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), aioduct::Error::Timeout));
}

#[tokio::test]
async fn test_request_timeout_overrides_client_timeout() {
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("delayed"))))
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .timeout(Duration::from_millis(10))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "delayed");
}
