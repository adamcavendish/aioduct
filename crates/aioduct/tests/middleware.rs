#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::{h1_server, h1_server_with};

#[tokio::test]
async fn test_middleware_adds_request_header() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-middleware")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-middleware"),
                    http::header::HeaderValue::from_static("injected"),
                );
            },
        )
        .build()
        .unwrap();

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

    let (addr, _counter) = h1_server().await;

    struct ResponseTagger {
        called: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for ResponseTagger {
        fn on_response(
            &self,
            response: &mut http::Response<aioduct::body::RequestBodySend>,
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(ResponseTagger {
            called: called.clone(),
        })
        .build()
        .unwrap();

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
    let (addr, _counter) = h1_server_with(|req| async move {
        let val = req
            .headers()
            .get("x-order")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(val))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-order"),
                    http::header::HeaderValue::from_static("first"),
                );
            },
        )
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-order"),
                    http::header::HeaderValue::from_static("second"),
                );
            },
        )
        .build()
        .unwrap();

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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(ErrorRecorder {
            error_seen: error_seen.clone(),
        })
        .build()
        .unwrap();

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

    let (final_addr, _counter) = h1_server().await;
    let (redirect_addr, _counter) = h1_server_with(move |_req| {
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(RedirectRecorder {
            redirect_seen: redirect_seen.clone(),
        })
        .build()
        .unwrap();

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
    let (addr, _counter) = h1_server_with(move |_req| {
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
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(RetryRecorder {
            retry_seen: retry_seen.clone(),
        })
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10)),
        )
        .build()
        .unwrap();

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

// ── Interaction Tests ──────────────────────────────────────────────────

/// Middleware sees every redirect in a 3-hop chain, recording from/to URIs.
#[tokio::test]
async fn middleware_on_redirect_sees_all_hops() {
    use std::sync::Mutex;

    // 3-hop chain: start -> hop1 -> hop2 -> final
    let (final_addr, _counter) = h1_server().await;

    let (hop2_addr, _counter) = h1_server_with({
        move |_req| {
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
        }
    })
    .await;

    let (hop1_addr, _counter) = h1_server_with({
        move |_req| {
            let target = format!("http://{hop2_addr}/");
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", target)
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            }
        }
    })
    .await;

    let (start_addr, _counter) = h1_server_with({
        move |_req| {
            let target = format!("http://{hop1_addr}/");
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(302)
                        .header("location", target)
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            }
        }
    })
    .await;

    struct HopRecorder {
        count: Arc<AtomicU32>,
        from_uris: Arc<Mutex<Vec<String>>>,
        to_uris: Arc<Mutex<Vec<String>>>,
    }

    impl aioduct::Middleware for HopRecorder {
        fn on_redirect(&self, _status: http::StatusCode, from: &http::Uri, to: &http::Uri) {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.from_uris.lock().unwrap().push(from.to_string());
            self.to_uris.lock().unwrap().push(to.to_string());
        }
    }

    let count = Arc::new(AtomicU32::new(0));
    let from_uris = Arc::new(Mutex::new(Vec::new()));
    let to_uris = Arc::new(Mutex::new(Vec::new()));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(HopRecorder {
            count: count.clone(),
            from_uris: from_uris.clone(),
            to_uris: to_uris.clone(),
        })
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{start_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);

    let n = count.load(Ordering::SeqCst);
    assert_eq!(n, 3, "expected 3 redirect hops, got {n}");

    let from = from_uris.lock().unwrap();
    let to = to_uris.lock().unwrap();
    assert_eq!(from.len(), 3);
    assert_eq!(to.len(), 3);

    // First redirect: start -> hop1
    assert!(
        from[0].contains(&start_addr.to_string()),
        "first from should be start"
    );
    assert!(
        to[0].contains(&hop1_addr.to_string()),
        "first to should be hop1"
    );
    // Second redirect: hop1 -> hop2
    assert!(
        from[1].contains(&hop1_addr.to_string()),
        "second from should be hop1"
    );
    assert!(
        to[1].contains(&hop2_addr.to_string()),
        "second to should be hop2"
    );
    // Third redirect: hop2 -> final
    assert!(
        from[2].contains(&hop2_addr.to_string()),
        "third from should be hop2"
    );
    assert!(
        to[2].contains(&final_addr.to_string()),
        "third to should be final"
    );
}

/// Middleware on_error fires when connect_timeout expires targeting a
/// non-routable IP.
#[tokio::test]
async fn middleware_on_error_for_connect_timeout() {
    struct TimeoutRecorder {
        error_seen: Arc<AtomicBool>,
        error_is_timeout: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for TimeoutRecorder {
        fn on_error(&self, err: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_seen.store(true, Ordering::SeqCst);
            if err.is_timeout() {
                self.error_is_timeout.store(true, Ordering::SeqCst);
            }
        }
    }

    let error_seen = Arc::new(AtomicBool::new(false));
    let error_is_timeout = Arc::new(AtomicBool::new(false));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(TimeoutRecorder {
            error_seen: error_seen.clone(),
            error_is_timeout: error_is_timeout.clone(),
        })
        .connect_timeout(Duration::from_millis(50))
        .build()
        .unwrap();

    // 192.0.2.1 is TEST-NET-1 (RFC 5737), non-routable
    let result = client.get("http://192.0.2.1:81/").unwrap().send().await;

    assert!(result.is_err(), "connect timeout should produce an error");
    assert!(
        error_seen.load(Ordering::SeqCst),
        "middleware on_error should have been called"
    );
    assert!(
        error_is_timeout.load(Ordering::SeqCst),
        "error should be classified as a timeout error"
    );
}

/// Middleware on_response fires for both the priming request and the
/// subsequent fresh cache hit.
#[tokio::test]
async fn middleware_with_cache_fresh_hit() {
    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=3600")
                .body(Full::new(Bytes::from("cached body")))
                .unwrap(),
        )
    })
    .await;

    struct CacheAwareMiddleware {
        response_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for CacheAwareMiddleware {
        fn on_response(
            &self,
            _response: &mut http::Response<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.response_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    let response_count = Arc::new(AtomicU32::new(0));
    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .middleware(CacheAwareMiddleware {
            response_count: response_count.clone(),
        })
        .build()
        .unwrap();

    let url = format!("http://{addr}/resource");

    // First request primes the cache
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached body");
    assert_eq!(
        response_count.load(Ordering::SeqCst),
        1,
        "on_response should fire for initial request"
    );

    // Second request is a fresh cache hit
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached body");
    assert_eq!(
        response_count.load(Ordering::SeqCst),
        2,
        "on_response should fire for fresh cache hit too"
    );
}

/// Middleware modifies the request URI path; the server receives the
/// modified path.
#[tokio::test]
async fn middleware_modifies_uri_in_on_request() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, uri: &http::Uri| {
                // Change the path in the request URI to /modified
                let modified_uri = format!(
                    "http://{}:{}/modified",
                    uri.authority().map(|a| a.host()).unwrap_or("127.0.0.1"),
                    uri.authority().and_then(|a| a.port_u16()).unwrap_or(80)
                );
                *req.uri_mut() = modified_uri.parse().unwrap();
            },
        )
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/original"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.text().await.unwrap(),
        "/modified",
        "middleware should have changed the URI path to /modified"
    );
}

/// When a retry budget is exhausted due to a transport error on the retry
/// attempt, on_retry fires first and on_error fires on the final failure.
#[tokio::test]
async fn middleware_on_retry_exhausted_fires_on_error() {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Raw TCP server: returns 500 on first connection, drops connection on retry
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let n = attempt_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: proper 500 response
                let _ = stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
            // Retry attempt: drop the stream immediately to cause a
            // transport error (connection reset / incomplete response).
            drop(stream);
        }
    });

    struct RetryExhaustedRecorder {
        retry_count: Arc<AtomicU32>,
        error_seen: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for RetryExhaustedRecorder {
        fn on_retry(
            &self,
            _err: &aioduct::Error,
            _uri: &http::Uri,
            _method: &http::Method,
            _attempt: u32,
        ) {
            self.retry_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_error(&self, _err: &aioduct::Error, _uri: &http::Uri, _method: &http::Method) {
            self.error_seen.store(true, Ordering::SeqCst);
        }
    }

    let retry_count = Arc::new(AtomicU32::new(0));
    let error_seen = Arc::new(AtomicBool::new(false));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(RetryExhaustedRecorder {
            retry_count: retry_count.clone(),
            error_seen: error_seen.clone(),
        })
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::from_millis(10)),
        )
        .build()
        .unwrap();

    let result = client.get(&format!("http://{addr}/")).unwrap().send().await;

    assert!(result.is_err(), "retry exhausted should result in an error");
    assert_eq!(
        retry_count.load(Ordering::SeqCst),
        1,
        "on_retry should fire once when the first attempt fails with 500"
    );
    assert!(
        error_seen.load(Ordering::SeqCst),
        "on_error should fire when the retry budget is exhausted by a transport error"
    );
}
