#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
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

#[cfg(feature = "json")]
#[tokio::test]
async fn test_json_request_and_response() {
    use http_body_util::BodyExt;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Payload {
        name: String,
        value: u32,
    }

    let addr = start_server_with(|req| async move {
        let content_type = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("missing").to_owned())
            .unwrap_or_else(|| "missing".to_owned());

        let body = req.into_body().collect().await.unwrap().to_bytes();
        let payload: Payload = serde_json::from_slice(&body).unwrap();

        let resp_body = serde_json::to_string(&Payload {
            name: payload.name.to_uppercase(),
            value: payload.value + 1,
        })
        .unwrap();

        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", content_type)
                .body(Full::new(Bytes::from(resp_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let input = Payload {
        name: "test".into(),
        value: 42,
    };

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .json(&input)
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );
    let output: Payload = resp.json().await.unwrap();
    assert_eq!(
        output,
        Payload {
            name: "TEST".into(),
            value: 43
        }
    );
}

#[tokio::test]
async fn test_bearer_auth() {
    let addr = start_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .bearer_auth("my-secret-token")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "Bearer my-secret-token");
}

#[tokio::test]
async fn test_basic_auth() {
    let addr = start_server_with(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .basic_auth("user", Some("pass"))
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "Basic dXNlcjpwYXNz");
}

#[tokio::test]
async fn test_query_params() {
    let addr = start_server_with(|req| async move {
        let query = req.uri().query().unwrap_or("none").to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(query))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/search"))
        .unwrap()
        .query(&[("q", "hello world"), ("page", "1")])
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "q=hello%20world&page=1");
}

#[tokio::test]
async fn test_default_user_agent() {
    let addr = start_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.starts_with("aioduct/"),
        "expected default User-Agent, got: {body}"
    );
}

#[tokio::test]
async fn test_custom_default_headers() {
    let addr = start_server_with(|req| async move {
        let custom = req
            .headers()
            .get("x-default")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom))))
    })
    .await;

    let mut headers = http::HeaderMap::new();
    headers.insert("x-default", "from-client".parse().unwrap());
    let client = Client::<TokioRuntime>::builder()
        .default_headers(headers)
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "from-client");
}

#[tokio::test]
async fn test_request_headers_override_defaults() {
    let addr = start_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header_str("user-agent", "custom-agent/1.0")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "custom-agent/1.0");
}

#[tokio::test]
async fn test_form_data() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body).to_string();
        let resp_body = format!("ct={ct}\nbody={body_str}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(resp_body))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .form(&[("username", "admin"), ("password", "s3cr&t=val")])
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("ct=application/x-www-form-urlencoded"),
        "expected form content-type, got: {body}"
    );
    assert!(
        body.contains("username=admin"),
        "expected username param, got: {body}"
    );
    assert!(
        body.contains("password=s3cr%26t%3Dval"),
        "expected encoded password, got: {body}"
    );
}

#[tokio::test]
async fn test_sse_stream() {
    let addr = start_server_with(|_req| async move {
        let sse_body =
            "event: greeting\ndata: hello\n\ndata: world\n\nevent: done\ndata: bye\nid: 3\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(sse_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut sse = resp.into_sse_stream();
    let mut events = Vec::new();
    while let Some(event) = sse.next().await {
        events.push(event.unwrap());
    }

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event.as_deref(), Some("greeting"));
    assert_eq!(events[0].data, "hello");
    assert_eq!(events[1].event, None);
    assert_eq!(events[1].data, "world");
    assert_eq!(events[2].event.as_deref(), Some("done"));
    assert_eq!(events[2].data, "bye");
    assert_eq!(events[2].id.as_deref(), Some("3"));
}

#[tokio::test]
async fn test_sse_multiline_data() {
    let addr = start_server_with(|_req| async move {
        let sse_body = "data: line1\ndata: line2\ndata: line3\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(sse_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut sse = resp.into_sse_stream();
    let event = sse.next().await.unwrap().unwrap();
    assert_eq!(event.data, "line1\nline2\nline3");
    assert!(sse.next().await.is_none());
}

#[tokio::test]
async fn test_sse_comments_and_retry() {
    let addr = start_server_with(|_req| async move {
        let sse_body = ": this is a comment\nretry: 5000\ndata: after comment\n\n";
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Full::new(Bytes::from(sse_body)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut sse = resp.into_sse_stream();
    let event = sse.next().await.unwrap().unwrap();
    assert_eq!(event.data, "after comment");
    assert_eq!(event.retry, Some(5000));
    assert!(sse.next().await.is_none());
}

#[tokio::test]
async fn test_retry_on_server_error() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("error")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("success"))))
            }
        }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(10)),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "success");
    assert_eq!(attempt.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_retry_exhausted() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(503)
                    .body(Full::new(Bytes::from("unavailable")))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10)),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(attempt.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_retry_disabled_on_status() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(500)
                    .body(Full::new(Bytes::from("error")))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .retry_on_status(false)
                .initial_backoff(Duration::from_millis(10)),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(attempt.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_client_default_retry() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n < 1 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(500)
                        .body(Full::new(Bytes::from("error")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("ok"))))
            }
        }
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10)),
        )
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_redirect_policy_none() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/target")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
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
    let final_addr = start_server().await;
    let addr = start_server_with(move |req| {
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

    let client = Client::<TokioRuntime>::builder()
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
async fn test_multipart_text_fields() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let ct = req
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body).to_string();
        let resp_body = format!("ct={ct}\nbody={body_str}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(resp_body))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let form = aioduct::Multipart::new()
        .text("field1", "value1")
        .text("field2", "value2");

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .multipart(form)
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("multipart/form-data; boundary="),
        "expected multipart content-type, got: {body}"
    );
    assert!(
        body.contains("name=\"field1\""),
        "expected field1, got: {body}"
    );
    assert!(body.contains("value1"), "expected value1, got: {body}");
    assert!(
        body.contains("name=\"field2\""),
        "expected field2, got: {body}"
    );
    assert!(body.contains("value2"), "expected value2, got: {body}");
}

#[tokio::test]
async fn test_multipart_file_upload() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body).to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body_str))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let form = aioduct::Multipart::new()
        .text("description", "test upload")
        .file("file", "hello.txt", "text/plain", "file contents here");

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .multipart(form)
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("filename=\"hello.txt\""),
        "expected filename, got: {body}"
    );
    assert!(
        body.contains("Content-Type: text/plain"),
        "expected file content-type, got: {body}"
    );
    assert!(
        body.contains("file contents here"),
        "expected file data, got: {body}"
    );
    assert!(
        body.contains("name=\"description\""),
        "expected description field, got: {body}"
    );
}

#[tokio::test]
async fn test_bytes_stream() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("chunk1chunk2chunk3"))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut stream = resp.into_bytes_stream();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk.unwrap());
    }

    assert_eq!(String::from_utf8(collected).unwrap(), "chunk1chunk2chunk3");
}

#[tokio::test]
async fn test_bytes_stream_empty() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let mut stream = resp.into_bytes_stream();
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn test_cookie_jar_stores_and_sends() {
    let addr = start_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();

        if req.uri().path() == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "session=abc123; Path=/")
                    .body(Full::new(Bytes::from("cookie set")))
                    .unwrap(),
            )
        } else {
            Ok(Response::new(Full::new(Bytes::from(format!(
                "cookies={cookie}"
            )))))
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = Client::<TokioRuntime>::builder().cookie_jar(jar).build();

    let resp = client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cookie set");

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body, "cookies=session=abc123");
}

#[tokio::test]
async fn test_cookie_jar_multiple_cookies() {
    let addr = start_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();

        match req.uri().path() {
            "/set1" => Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "a=1")
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap(),
            ),
            "/set2" => Ok(Response::builder()
                .header("set-cookie", "b=2")
                .body(Full::new(Bytes::from("ok")))
                .unwrap()),
            _ => Ok(Response::new(Full::new(Bytes::from(format!(
                "cookies={cookie}"
            ))))),
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = Client::<TokioRuntime>::builder().cookie_jar(jar).build();

    client
        .get(&format!("http://{addr}/set1"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    client
        .get(&format!("http://{addr}/set2"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.contains("a=1"), "expected cookie a, got: {body}");
    assert!(body.contains("b=2"), "expected cookie b, got: {body}");
}

#[tokio::test]
async fn test_no_cookie_jar_no_cookies() {
    let addr = start_server_with(|req| async move {
        let has_cookie = req.headers().contains_key("cookie");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "has_cookie={has_cookie}"
        )))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "has_cookie=false");
}

#[tokio::test]
async fn test_http_proxy() {
    let proxy_addr = start_server_with(|req| async move {
        let uri = req.uri().to_string();
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let body = format!("proxied: uri={uri} host={host}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build();

    let resp = client
        .get("http://example.com/path")
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("proxied:"),
        "expected proxied response, got: {body}"
    );
    assert!(body.contains("/path"), "expected path in URI, got: {body}");
}

#[tokio::test]
async fn test_streaming_body_upload() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(body)))
    })
    .await;

    let chunks: Vec<Result<hyper::body::Frame<Bytes>, aioduct::Error>> = vec![
        Ok(hyper::body::Frame::data(Bytes::from("hello "))),
        Ok(hyper::body::Frame::data(Bytes::from("streaming "))),
        Ok(hyper::body::Frame::data(Bytes::from("world"))),
    ];

    let stream = futures_util::stream::iter(chunks);
    let stream_body: aioduct::HyperBody = http_body_util::StreamBody::new(stream).boxed();

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello streaming world");
}

#[tokio::test]
async fn test_streaming_body_from_request_body() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(body)))
    })
    .await;

    let data = "buffered body content";
    let client = Client::<TokioRuntime>::new();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body(data)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "buffered body content");
}

#[tokio::test]
async fn test_chunk_download() {
    let data = "abcdefghijklmnopqrstuvwxyz0123456789";

    let addr = start_server_with(move |req| async move {
        let total = data.len();
        if req.method() == http::Method::HEAD {
            return Ok::<_, Infallible>(
                Response::builder()
                    .header("content-length", total.to_string())
                    .header("accept-ranges", "bytes")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            );
        }

        if let Some(range) = req.headers().get("range") {
            let range_str = range.to_str().unwrap();
            let range_str = range_str.trim_start_matches("bytes=");
            let parts: Vec<&str> = range_str.split('-').collect();
            let start: usize = parts[0].parse().unwrap();
            let end: usize = parts[1].parse().unwrap();
            let slice = &data[start..=end];
            return Ok(Response::builder()
                .status(206)
                .header("content-range", format!("bytes {start}-{end}/{total}"))
                .body(Full::new(Bytes::from(slice.to_owned())))
                .unwrap());
        }

        Ok(Response::new(Full::new(Bytes::from(data))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(result.total_size, 36);
    assert_eq!(
        String::from_utf8(result.data.to_vec()).unwrap(),
        "abcdefghijklmnopqrstuvwxyz0123456789"
    );
}

#[tokio::test]
async fn test_chunk_download_fallback_no_range() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("no range support"))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let result = client
        .chunk_download(&format!("http://{addr}/"))
        .chunks(4)
        .download()
        .await
        .unwrap();

    assert_eq!(
        String::from_utf8(result.data.to_vec()).unwrap(),
        "no range support"
    );
}

// === Comprehensive test suite ===

#[tokio::test]
async fn test_put_request() {
    use http_body_util::BodyExt;

    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        let resp_body = format!("method={method} body={}", String::from_utf8_lossy(&body));
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(resp_body))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .put(&format!("http://{addr}/"))
        .unwrap()
        .body("update data")
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("method=PUT"), "expected PUT, got: {body}");
    assert!(
        body.contains("body=update data"),
        "expected body, got: {body}"
    );
}

#[tokio::test]
async fn test_patch_request() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .patch(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "PATCH");
}

#[tokio::test]
async fn test_delete_request() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .delete(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "DELETE");
}

#[tokio::test]
async fn test_head_request() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(
            Response::builder()
                .header("x-method", method)
                .header("content-length", "1000")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .head(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.headers().get("x-method").unwrap().to_str().unwrap(),
        "HEAD"
    );
    assert_eq!(resp.content_length(), Some(1000));
}

#[tokio::test]
async fn test_connection_refused() {
    let client = Client::<TokioRuntime>::new();
    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_empty_body_response() {
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "");
}

#[tokio::test]
async fn test_large_body() {
    let data = "x".repeat(100_000);
    let data_clone = data.clone();

    let addr = start_server_with(move |_req| {
        let data = data_clone.clone();
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(data)))) }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body.len(), 100_000);
}

#[tokio::test]
async fn test_query_params_with_existing_query() {
    let addr = start_server_with(|req| async move {
        let query = req.uri().query().unwrap_or("").to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(query))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/?existing=1"))
        .unwrap()
        .query(&[("extra", "2")])
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("existing=1"),
        "expected existing, got: {body}"
    );
    assert!(body.contains("extra=2"), "expected extra, got: {body}");
}

#[tokio::test]
async fn test_cookie_jar_same_host_shared() {
    let jar = aioduct::CookieJar::new();

    let addr1 = start_server_with(|req| async move {
        let cookie = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(
            Response::builder()
                .header("set-cookie", "session=abc123")
                .body(Full::new(Bytes::from(cookie)))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::builder().cookie_jar(jar).build();

    // First request stores the cookie
    let resp1 = client
        .get(&format!("http://{addr1}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body1 = resp1.text().await.unwrap();
    assert!(body1.is_empty(), "first request should have no cookie");

    // Second request to same host should include the stored cookie
    let resp2 = client
        .get(&format!("http://{addr1}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body2 = resp2.text().await.unwrap();
    assert!(
        body2.contains("session=abc123"),
        "second request should have cookie, got: {body2}"
    );
}

#[tokio::test]
async fn test_no_default_headers() {
    let addr = start_server_with(|req| async move {
        let ua = req
            .headers()
            .get("user-agent")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_else(|| "none".to_owned());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(ua))))
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .no_default_headers()
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "none");
}

#[tokio::test]
async fn test_response_content_length() {
    let body = "x".repeat(42);
    let body_clone = body.clone();
    let addr = start_server_with(move |_req| {
        let body = body_clone.clone();
        async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body)))) }
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(42));
}

#[tokio::test]
async fn test_response_version() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.version(), http::Version::HTTP_11);
}

#[tokio::test]
async fn test_client_clone_shares_pool() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::new();
    let cloned = client.clone();

    let resp1 = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp1.text().await.unwrap();

    let resp2 = cloned
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp2.status(), http::StatusCode::OK);
    let body = resp2.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn test_custom_method() {
    let addr = start_server_with(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .request(http::Method::OPTIONS, &format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "OPTIONS");
}

#[tokio::test]
async fn test_multiple_headers_same_name() {
    let addr = start_server_with(|req| async move {
        let values: Vec<String> = req
            .headers()
            .get_all("x-multi")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        let body = values.join(",");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let mut headers = http::HeaderMap::new();
    headers.append("x-multi", "value1".parse().unwrap());
    headers.append("x-multi", "value2".parse().unwrap());

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .headers(headers)
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("value1"), "expected value1, got: {body}");
    assert!(body.contains("value2"), "expected value2, got: {body}");
}

#[tokio::test]
async fn test_concurrent_requests() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::new();

    let mut handles = Vec::new();
    for _ in 0..10 {
        let client = client.clone();
        let url = format!("http://{addr}/");
        handles.push(tokio::spawn(async move {
            client
                .get(&url)
                .unwrap()
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }));
    }

    for handle in handles {
        let body = handle.await.unwrap();
        assert_eq!(body, "hello aioduct");
    }
}

// ── Decompression tests ──────────────────────────────────────────────────────

#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_decompression() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"hello compressed world").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "gzip")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(!resp.headers().contains_key("content-encoding"));
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello compressed world");
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_accept_encoding_header() {
    let handler = |req: Request<hyper::body::Incoming>| async move {
        let accept = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept))))
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::new();
    let text = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(text.contains("gzip"));
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_no_decompression_passthrough() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"raw gzip data").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "gzip")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::builder().no_decompression().build();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(resp.headers().contains_key("content-encoding"));
    let bytes = resp.bytes().await.unwrap();
    // Should be raw gzip, not decompressed
    assert_ne!(bytes.as_ref(), b"raw gzip data");
}

#[cfg(feature = "deflate")]
#[tokio::test]
async fn test_deflate_decompression() {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    let handler = |_req: Request<hyper::body::Incoming>| async {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"deflate test payload").unwrap();
        let compressed = encoder.finish().unwrap();

        let resp = Response::builder()
            .header("content-encoding", "deflate")
            .body(Full::new(Bytes::from(compressed)))
            .unwrap();
        Ok::<_, Infallible>(resp)
    };
    let addr = start_server_with(handler).await;
    let client = Client::<TokioRuntime>::new();
    let text = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert_eq!(text, "deflate test payload");
}

#[tokio::test]
async fn test_proxy_settings_no_proxy_bypass() {
    // Set up a "proxy" server that labels responses
    let proxy_addr = start_server_with(|req| async move {
        let uri = req.uri().to_string();
        let body = format!("proxied: {uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    // Set up the actual target server
    let target_addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new(&format!("{}", target_addr.ip())));

    let client = Client::<TokioRuntime>::builder()
        .proxy_settings(settings)
        .build();

    // Request to the bypassed host goes direct
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "direct");

    // Request to a non-bypassed host goes through proxy
    let resp = client
        .get("http://example.com/test")
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("proxied:"), "expected proxy, got: {body}");
}

#[tokio::test]
async fn test_no_proxy_wildcard_bypasses_all() {
    let target_addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    let settings =
        aioduct::ProxySettings::all(aioduct::ProxyConfig::http("http://127.0.0.1:9999").unwrap())
            .no_proxy(aioduct::NoProxy::new("*"));

    let client = Client::<TokioRuntime>::builder()
        .proxy_settings(settings)
        .build();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "direct");
}

#[tokio::test]
async fn test_no_proxy_domain_suffix_matching() {
    let no_proxy = aioduct::NoProxy::new(".example.com, localhost");

    // Direct matches
    assert!(!no_proxy.matches("example.com")); // no leading dot, exact doesn't match
    assert!(no_proxy.matches("foo.example.com"));
    assert!(no_proxy.matches("bar.baz.example.com"));
    assert!(no_proxy.matches("localhost"));

    // Non-matches
    assert!(!no_proxy.matches("notexample.com"));
    assert!(!no_proxy.matches("other.com"));
}

#[tokio::test]
async fn test_no_proxy_bare_domain_matches_subdomains() {
    let no_proxy = aioduct::NoProxy::new("example.com");

    assert!(no_proxy.matches("example.com"));
    assert!(no_proxy.matches("foo.example.com"));
    assert!(!no_proxy.matches("notexample.com"));
}

#[tokio::test]
async fn test_custom_resolver() {
    use std::pin::Pin;

    let target_addr = start_server().await;

    let resolver_addr = target_addr;
    let client = Client::<TokioRuntime>::builder()
        .resolver(
            move |_host: &str,
                  _port: u16|
                  -> Pin<
                Box<dyn std::future::Future<Output = std::io::Result<std::net::SocketAddr>> + Send>,
            > { Box::pin(async move { Ok(resolver_addr) }) },
        )
        .build();

    // Request to a fake host, but resolver redirects to our test server
    let resp = client
        .get("http://fake-host.invalid/")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_tcp_keepalive() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::builder()
        .tcp_keepalive(Duration::from_secs(60))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_local_address_binding() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::builder()
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_http2_config_accepted() {
    let addr = start_server().await;
    let client = Client::<TokioRuntime>::builder()
        .http2(
            aioduct::Http2Config::new()
                .initial_stream_window_size(1024 * 1024)
                .initial_connection_window_size(2 * 1024 * 1024)
                .max_frame_size(32_768)
                .adaptive_window(true)
                .keep_alive_interval(Duration::from_secs(30))
                .keep_alive_timeout(Duration::from_secs(10))
                .keep_alive_while_idle(true)
                .max_header_list_size(8192)
                .max_send_buf_size(1024 * 1024)
                .max_concurrent_reset_streams(100),
        )
        .build();

    // HTTP/1 request still works with h2 config set (config only applies to h2 connections)
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_socks5_proxy() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let target_addr = start_server().await;

    // Minimal SOCKS5 proxy server
    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut client, _) = socks_listener.accept().await.unwrap();

            tokio::spawn(async move {
                // Read greeting
                let mut buf = [0u8; 256];
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x05); // SOCKS5

                // Reply: no auth
                client.write_all(&[0x05, 0x00]).await.unwrap();

                // Read connect request
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);
                assert_eq!(buf[0], 0x05); // SOCKS5
                assert_eq!(buf[1], 0x01); // CONNECT
                assert_eq!(buf[3], 0x03); // Domain

                let domain_len = buf[4] as usize;
                let port_offset = 5 + domain_len;
                let port = ((buf[port_offset] as u16) << 8) | (buf[port_offset + 1] as u16);

                // Connect to target
                let target = format!("127.0.0.1:{port}");
                let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();

                // Reply: success, bound to 0.0.0.0:0
                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                    .await
                    .unwrap();

                // Bidirectional relay
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let client = Client::<TokioRuntime>::builder()
        .proxy(aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap())
        .build();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_socks5_proxy_with_auth() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let target_addr = start_server().await;

    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut client, _) = socks_listener.accept().await.unwrap();

            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x05);

                // Require username/password auth
                client.write_all(&[0x05, 0x02]).await.unwrap();

                // Read auth sub-negotiation
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 3);
                assert_eq!(buf[0], 0x01); // sub-version
                let ulen = buf[1] as usize;
                let username = String::from_utf8_lossy(&buf[2..2 + ulen]).to_string();
                let plen = buf[2 + ulen] as usize;
                let password = String::from_utf8_lossy(&buf[3 + ulen..3 + ulen + plen]).to_string();

                if username == "testuser" && password == "testpass" {
                    client.write_all(&[0x01, 0x00]).await.unwrap();
                } else {
                    client.write_all(&[0x01, 0x01]).await.unwrap();
                    return;
                }

                // Read connect request
                let n = client.read(&mut buf).await.unwrap();
                assert!(n >= 7);

                let domain_len = buf[4] as usize;
                let port_offset = 5 + domain_len;
                let port = ((buf[port_offset] as u16) << 8) | (buf[port_offset + 1] as u16);

                let target = format!("127.0.0.1:{port}");
                let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();

                client
                    .write_all(&[0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])
                    .await
                    .unwrap();

                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let client = Client::<TokioRuntime>::builder()
        .proxy(
            aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}"))
                .unwrap()
                .basic_auth("testuser", "testpass"),
        )
        .build();

    let resp = client
        .get(&format!("http://localhost:{}/", target_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_middleware_adds_request_header() {
    let addr = start_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-middleware")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::HyperBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-middleware"),
                    http::header::HeaderValue::from_static("injected"),
                );
            },
        )
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "injected");
}

#[tokio::test]
async fn test_middleware_modifies_response_header() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let addr = start_server().await;

    struct ResponseTagger {
        called: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for ResponseTagger {
        fn on_response(&self, response: &mut http::Response<aioduct::HyperBody>, _uri: &http::Uri) {
            self.called.store(true, Ordering::SeqCst);
            response.headers_mut().insert(
                http::header::HeaderName::from_static("x-from-middleware"),
                http::header::HeaderValue::from_static("yes"),
            );
        }
    }

    let called = Arc::new(AtomicBool::new(false));
    let client = Client::<TokioRuntime>::builder()
        .middleware(ResponseTagger {
            called: called.clone(),
        })
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(called.load(Ordering::SeqCst));
    assert_eq!(
        resp.headers()
            .get("x-from-middleware")
            .unwrap()
            .to_str()
            .unwrap(),
        "yes"
    );
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn test_multiple_middleware_ordering() {
    let addr = start_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-order")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::HyperBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-order"),
                    http::header::HeaderValue::from_static("first"),
                );
            },
        )
        .middleware(
            |req: &mut http::Request<aioduct::HyperBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-order"),
                    http::header::HeaderValue::from_static("second"),
                );
            },
        )
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "second");
}

#[tokio::test]
async fn test_upgrade_websocket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);

        let conn = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                io,
                hyper::service::service_fn(
                    |mut req: hyper::Request<hyper::body::Incoming>| async move {
                        if req.headers().get("upgrade").map(|v| v.as_bytes()) == Some(b"websocket")
                        {
                            tokio::spawn(async move {
                                if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
                                    let mut upgraded = aioduct::Upgraded::from(upgraded);
                                    let mut buf = vec![0u8; 64];
                                    let n =
                                        AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
                                    AsyncWriteExt::write_all(&mut upgraded, &buf[..n])
                                        .await
                                        .unwrap();
                                }
                            });

                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(101)
                                    .header("connection", "Upgrade")
                                    .header("upgrade", "websocket")
                                    .body(Full::new(Bytes::new()))
                                    .unwrap(),
                            )
                        } else {
                            Ok(Response::new(Full::new(Bytes::from("not an upgrade"))))
                        }
                    },
                ),
            )
            .with_upgrades();

        conn.await.unwrap();
    });

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .upgrade()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    let mut upgraded = resp.upgrade().await.unwrap();

    AsyncWriteExt::write_all(&mut upgraded, b"hello upgrade")
        .await
        .unwrap();

    let mut buf = vec![0u8; 64];
    let n = AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello upgrade");
}

// --- Ported from reqwest: redirect edge-case tests ---

#[tokio::test]
async fn test_redirect_301_and_302_and_303_changes_post_to_get() {
    let codes = [301u16, 302, 303];
    for &code in &codes {
        let addr = start_server_with(move |req| async move {
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

        let client = Client::<TokioRuntime>::new();
        let url = format!("http://{addr}/{code}");
        let resp = client.post(&url).unwrap().send().await.unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.headers().get("server").unwrap(), "test-dst");
    }
}

#[tokio::test]
async fn test_redirect_307_and_308_replays_post_body() {
    use http_body_util::BodyExt;

    let codes = [307u16, 308];
    for &code in &codes {
        let addr = start_server_with(move |req| async move {
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

        let client = Client::<TokioRuntime>::new();
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
    let final_addr = start_server_with(|req| async move {
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

    let redirect_addr = start_server_with(move |req| {
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

    let client = Client::<TokioRuntime>::new();
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
    use http_body_util::BodyExt;

    let codes = [301u16, 302, 303];
    for &code in &codes {
        let addr = start_server_with(move |req| async move {
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

        let client = Client::<TokioRuntime>::new();
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
    let addr = start_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "http://www.yikes{KABOOM}")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
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

    let client = Client::<TokioRuntime>::new();
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
    let addr = start_server_with(|req| async move {
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

    let client = Client::<TokioRuntime>::builder().max_redirects(1).build();
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
    let addr = start_server_with(|req| async move {
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

    let client = Client::<TokioRuntime>::builder()
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
    let addr = start_server_with(|req| async move {
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

    let client = Client::<TokioRuntime>::builder().referer(true).build();
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
    let addr = start_server_with(|req| async move {
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

    let client = Client::<TokioRuntime>::new();
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

// --- Ported from reqwest: client, compression, cookie edge-case tests ---

#[tokio::test]
async fn test_get_no_content_headers() {
    let addr = start_server_with(|req| async move {
        assert_eq!(req.method(), "GET");
        assert!(
            req.headers().get("content-length").is_none(),
            "GET should not have content-length"
        );
        assert!(
            req.headers().get("content-type").is_none(),
            "GET should not have content-type"
        );
        assert!(
            req.headers().get("transfer-encoding").is_none(),
            "GET should not have transfer-encoding"
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_gzip_empty_body_head_request() {
    let addr = start_server_with(|req| async move {
        assert_eq!(req.method(), "HEAD");
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-encoding", "gzip")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .head(&format!("http://{addr}/gzip"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "");
}

#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_custom_accept_encoding_preserved() {
    let addr = start_server_with(|req| async move {
        let accept_encoding = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept_encoding))))
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .header(
            http::header::ACCEPT_ENCODING,
            http::header::HeaderValue::from_static("identity"),
        )
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "identity");
}

#[tokio::test]
async fn test_cookie_store_max_age_zero() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();
    let addr = start_server_with(move |req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("set-cookie", "key=val; Max-Age=0")
                        .body(Full::new(Bytes::from("set")))
                        .unwrap(),
                )
            } else {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = Client::<TokioRuntime>::builder().cookie_jar(jar).build();

    client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=none",
        "cookie with Max-Age=0 should not be sent"
    );
}

#[tokio::test]
async fn test_cookie_store_expired() {
    let request_count = Arc::new(AtomicU32::new(0));
    let request_count_clone = request_count.clone();
    let addr = start_server_with(move |req| {
        let count = request_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header(
                            "set-cookie",
                            "key=val; Expires=Wed, 21 Oct 2015 07:28:00 GMT",
                        )
                        .body(Full::new(Bytes::from("set")))
                        .unwrap(),
                )
            } else {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = Client::<TokioRuntime>::builder().cookie_jar(jar).build();

    client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=none",
        "cookie with past Expires should not be sent"
    );
}

#[tokio::test]
async fn test_cookie_store_path_scoping() {
    let addr = start_server_with(|req| async move {
        if req.uri().path() == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "key=val; Path=/subpath")
                    .body(Full::new(Bytes::from("set")))
                    .unwrap(),
            )
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap().to_owned());
            let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
            Ok(Response::new(Full::new(Bytes::from(body))))
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = Client::<TokioRuntime>::builder().cookie_jar(jar).build();

    client
        .get(&format!("http://{addr}/set"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=none",
        "cookie with Path=/subpath should not be sent to /"
    );

    let resp = client
        .get(&format!("http://{addr}/subpath"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "cookie=key=val",
        "cookie with Path=/subpath should be sent to /subpath"
    );
}

#[tokio::test]
async fn test_cookie_store_overwrite() {
    let addr = start_server_with(|req| async move {
        match req.uri().path() {
            "/set1" => Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "key=val1")
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap(),
            ),
            "/set2" => Ok(Response::builder()
                .header("set-cookie", "key=val2")
                .body(Full::new(Bytes::from("ok")))
                .unwrap()),
            _ => {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = Client::<TokioRuntime>::builder().cookie_jar(jar).build();

    client
        .get(&format!("http://{addr}/set1"))
        .unwrap()
        .send()
        .await
        .unwrap();
    client
        .get(&format!("http://{addr}/set2"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/check"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(body, "cookie=key=val2");
}

// --- Ported from reqwest: timeout edge-case tests ---

#[tokio::test]
async fn test_read_timeout_does_not_apply_to_headers() {
    // Note: aioduct's read_timeout only applies to body reads, not header wait.
    // Use request timeout for header wait timeouts.
    let addr = start_server_with(|_req| async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow headers"))))
    })
    .await;

    let client = Client::<TokioRuntime>::builder()
        .read_timeout(Duration::from_millis(100))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "slow headers");
}

#[tokio::test]
async fn test_read_timeout_applies_to_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = stream.write_all(b"world").await;
    });

    let client = Client::<TokioRuntime>::builder()
        .read_timeout(Duration::from_millis(100))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body_result = resp.text().await;
    assert!(
        body_result.is_err(),
        "read_timeout should fire on slow body chunks"
    );
}

#[tokio::test]
async fn test_read_timeout_allows_slow_but_steady_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;

        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        for i in 0..3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let chunk = format!("1\r\n{i}\r\n");
            stream.write_all(chunk.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        }

        stream.write_all(b"0\r\n\r\n").await.unwrap();
        stream.flush().await.unwrap();
    });

    let client = Client::<TokioRuntime>::builder()
        .read_timeout(Duration::from_millis(200))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "012", "slow-but-within-threshold body should succeed");
}

#[tokio::test]
async fn test_content_length_preserved_through_timeout() {
    let addr = start_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "5")
                .body(Full::new(Bytes::from("hello")))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.content_length(), Some(5));
}

#[tokio::test]
async fn test_connect_timeout() {
    let client = Client::<TokioRuntime>::builder()
        .connect_timeout(Duration::from_millis(100))
        .build();

    let start = tokio::time::Instant::now();
    let result = client
        .get("http://192.0.2.1:81/slow")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    assert!(result.is_err(), "connect_timeout should fire");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "should timeout quickly, not wait for request timeout"
    );
}
