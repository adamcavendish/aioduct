#![cfg(feature = "tokio")]

mod common;
use common::*;

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBoxBody>, _uri: &http::Uri| {
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
        fn on_response(
            &self,
            response: &mut http::Response<aioduct::body::RequestBoxBody>,
            _uri: &http::Uri,
        ) {
            self.called.store(true, Ordering::SeqCst);
            response.headers_mut().insert(
                http::header::HeaderName::from_static("x-from-middleware"),
                http::header::HeaderValue::from_static("yes"),
            );
        }
    }

    let called = Arc::new(AtomicBool::new(false));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBoxBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-order"),
                    http::header::HeaderValue::from_static("first"),
                );
            },
        )
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBoxBody>, _uri: &http::Uri| {
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
async fn test_middleware_on_error_callback() {
    use std::sync::atomic::AtomicBool;

    struct ErrorRecorder {
        error_seen: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for ErrorRecorder {
        fn on_error(&self, _err: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_seen.store(true, Ordering::SeqCst);
        }
    }

    let error_seen = Arc::new(AtomicBool::new(false));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(ErrorRecorder {
            error_seen: error_seen.clone(),
        })
        .build();

    // Connect to a port that will refuse connection
    let result = client.get("http://127.0.0.1:1/").unwrap().send().await;
    assert!(result.is_err());
    assert!(
        error_seen.load(Ordering::SeqCst),
        "middleware on_error should have been called"
    );
}
#[tokio::test]
async fn test_middleware_on_redirect_callback() {
    use std::sync::atomic::AtomicBool;

    struct RedirectRecorder {
        redirect_seen: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for RedirectRecorder {
        fn on_redirect(&self, _status: http::StatusCode, _from: &http::Uri, _to: &http::Uri) {
            self.redirect_seen.store(true, Ordering::SeqCst);
        }
    }

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

    let redirect_seen = Arc::new(AtomicBool::new(false));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(RedirectRecorder {
            redirect_seen: redirect_seen.clone(),
        })
        .build();

    let resp = client
        .get(&format!("http://{redirect_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        redirect_seen.load(Ordering::SeqCst),
        "middleware on_redirect should have been called"
    );
}
#[tokio::test]
async fn test_middleware_on_retry_callback() {
    use std::sync::atomic::AtomicBool;

    struct RetryRecorder {
        retry_seen: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for RetryRecorder {
        fn on_retry(
            &self,
            _err: &aioduct::Error,
            _uri: &http::Uri,
            _method: &http::Method,
            _attempt: u32,
        ) {
            self.retry_seen.store(true, Ordering::SeqCst);
        }
    }

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

    let retry_seen = Arc::new(AtomicBool::new(false));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .middleware(RetryRecorder {
            retry_seen: retry_seen.clone(),
        })
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
    assert!(
        retry_seen.load(Ordering::SeqCst),
        "middleware on_retry should have been called"
    );
}
