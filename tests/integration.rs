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
