#![cfg(any(feature = "tokio", feature = "compio"))]

use std::convert::Infallible;
use std::sync::Arc;
#[cfg(feature = "tokio")]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "tokio")]
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use http_body_util::Full;
#[cfg(feature = "tokio")]
use hyper::Response;

#[cfg(feature = "compio")]
use aioduct::HttpEngineLocal;
#[cfg(feature = "tokio")]
use aioduct::HttpEngineSend;
#[cfg(feature = "tokio")]
use aioduct::runtime::TokioRuntime;
#[cfg(feature = "compio")]
use aioduct::runtime::compio_rt::{CompioRuntime, TcpConnector as CompioTcpConnector};
#[cfg(feature = "tokio")]
use aioduct::runtime::tokio_rt::TcpConnector;

#[cfg(feature = "tokio")]
use aioduct_test_server::h1::{h1_server, h1_server_with};

#[cfg(feature = "tokio")]
use aioduct_test_server::h2::h2_server;

#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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
#[cfg(feature = "tokio")]
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

// ── Interaction Tests: Transport, SSE, Local, Streaming ────────────────────────

/// Middleware `on_request` and `on_response` hooks both fire when using
/// HTTP/2 prior knowledge.
#[cfg(feature = "tokio")]
#[tokio::test]
async fn middleware_with_h2_transport() {
    let (addr, _counter) = h2_server().await;

    struct TransportCountingMiddleware {
        on_request_count: Arc<AtomicU32>,
        on_response_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for TransportCountingMiddleware {
        fn on_request(
            &self,
            _req: &mut http::Request<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.on_request_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_response(
            &self,
            _resp: &mut http::Response<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.on_response_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    let on_request_count = Arc::new(AtomicU32::new(0));
    let on_response_count = Arc::new(AtomicU32::new(0));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .http2_prior_knowledge()
        .middleware(TransportCountingMiddleware {
            on_request_count: on_request_count.clone(),
            on_response_count: on_response_count.clone(),
        })
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(
        resp.version(),
        http::Version::HTTP_2,
        "response should use HTTP/2"
    );
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");

    assert_eq!(
        on_request_count.load(Ordering::SeqCst),
        1,
        "on_request should fire once on H2 transport"
    );
    assert_eq!(
        on_response_count.load(Ordering::SeqCst),
        1,
        "on_response should fire once on H2 transport"
    );
}

/// Middleware `on_response` fires before the SSE stream is consumed.
/// Response headers from middleware survive `into_sse_stream()`.
#[cfg(feature = "tokio")]
#[tokio::test]
async fn middleware_with_sse_streaming() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(Full::new(Bytes::from("data: hello sse\n\n")))
                .unwrap(),
        )
    })
    .await;

    struct SseMiddleware {
        on_response_count: Arc<AtomicU32>,
        on_request_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for SseMiddleware {
        fn on_request(
            &self,
            _req: &mut http::Request<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.on_request_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_response(
            &self,
            response: &mut http::Response<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.on_response_count.fetch_add(1, Ordering::SeqCst);
            response.headers_mut().insert(
                http::header::HeaderName::from_static("x-sse-from-middleware"),
                http::header::HeaderValue::from_static("tagged"),
            );
        }
    }

    let on_request_count = Arc::new(AtomicU32::new(0));
    let on_response_count = Arc::new(AtomicU32::new(0));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(SseMiddleware {
            on_request_count: on_request_count.clone(),
            on_response_count: on_response_count.clone(),
        })
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);

    // on_response fires during dispatch, before the caller gets the Response
    assert_eq!(
        on_request_count.load(Ordering::SeqCst),
        1,
        "on_request should fire before SSE stream consumption"
    );
    assert_eq!(
        on_response_count.load(Ordering::SeqCst),
        1,
        "on_response should fire before SSE stream consumption"
    );

    // Headers survive the middleware chain — verify before consuming the body
    assert_eq!(
        resp.headers()
            .get("x-sse-from-middleware")
            .unwrap()
            .to_str()
            .unwrap(),
        "tagged",
        "middleware-added header should be visible on the response"
    );

    // Consume via into_sse_stream() — this takes ownership of the response
    let mut sse = resp.into_sse_stream();
    let event = sse.next().await.unwrap().unwrap();
    match event {
        aioduct::sse::SseEvent::Message(m) => {
            assert_eq!(m.data, "hello sse");
        }
        other => panic!("expected SSE message, got {other:?}"),
    }
    assert!(sse.next().await.is_none(), "SSE stream should be exhausted");
}

/// Middleware `apply_request_local` modifies headers that the server sees.
/// When middleware does NOT replace the body, the original body is preserved
/// (not accidentally consumed by the sentinel in the middleware bridge).
#[cfg(feature = "compio")]
#[test]
fn middleware_apply_request_local_full_path() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let io = aioduct_test_server::TokioIo::new(stream);
                    let svc = hyper::service::service_fn(
                        |req: hyper::Request<hyper::body::Incoming>| async move {
                            let header_val = req
                                .headers()
                                .get("x-local-middleware")
                                .map(|v| v.to_str().unwrap().to_string())
                                .unwrap_or_else(|| "missing".to_string());
                            let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                            let body_str = String::from_utf8_lossy(&body_bytes);
                            Ok::<_, Infallible>(hyper::Response::new(Full::new(Bytes::from(
                                format!("{header_val}|{body_str}"),
                            ))))
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
    });
    let addr: std::net::SocketAddr = rx.recv().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        // ── Part 1: middleware modifies headers, server sees them ───

        struct HeaderInjector {
            fired: Arc<AtomicBool>,
        }

        impl aioduct::Middleware for HeaderInjector {
            fn on_request(
                &self,
                req: &mut http::Request<aioduct::body::RequestBodySend>,
                _uri: &http::Uri,
            ) {
                self.fired.store(true, Ordering::SeqCst);
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-local-middleware"),
                    http::header::HeaderValue::from_static("yes-from-local"),
                );
            }
        }

        let fired = Arc::new(AtomicBool::new(false));
        let client = HttpEngineLocal::<CompioRuntime, CompioTcpConnector>::builder()
            .middleware(HeaderInjector {
                fired: fired.clone(),
            })
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert!(
            fired.load(Ordering::SeqCst),
            "middleware on_request should fire"
        );
        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert!(
            body.starts_with("yes-from-local|"),
            "server should receive middleware header, got: {body}"
        );

        // ── Part 2: middleware modifies metadata only, body preserved ───

        struct MetadataOnlyMiddleware {
            fired: Arc<AtomicBool>,
        }

        impl aioduct::Middleware for MetadataOnlyMiddleware {
            fn on_request(
                &self,
                req: &mut http::Request<aioduct::body::RequestBodySend>,
                _uri: &http::Uri,
            ) {
                self.fired.store(true, Ordering::SeqCst);
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-local-middleware"),
                    http::header::HeaderValue::from_static("metadata-only"),
                );
            }
        }

        let fired2 = Arc::new(AtomicBool::new(false));
        let client2 = HttpEngineLocal::<CompioRuntime, CompioTcpConnector>::builder()
            .middleware(MetadataOnlyMiddleware {
                fired: fired2.clone(),
            })
            .build_local()
            .unwrap();

        let resp2 = client2
            .get_local(&format!("http://{addr}/echo-body"))
            .unwrap()
            .body("original-body-content")
            .send()
            .await
            .unwrap();

        assert!(
            fired2.load(Ordering::SeqCst),
            "metadata-only middleware should fire"
        );
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let body2 = resp2.text().await.unwrap();
        assert!(
            body2.starts_with("metadata-only|"),
            "server should receive middleware header, got: {body2}"
        );
        assert!(
            body2.contains("original-body-content"),
            "body should be preserved when middleware only modifies metadata, got: {body2}"
        );
    });
}

/// Middleware `on_request` fires and header modifications reach the server
/// when using a streaming request body. The streaming body is preserved.
#[cfg(feature = "tokio")]
#[tokio::test]
async fn middleware_with_streaming_request_body() {
    let (addr, _counter) = h1_server_with(|req| async move {
        use http_body_util::BodyExt;
        let middleware_header = req
            .headers()
            .get("x-streaming-header")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_else(|| "absent".to_string());
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&body_bytes);
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "header={middleware_header}|body={body_str}"
        )))))
    })
    .await;

    struct StreamingRequestMiddleware {
        on_request_count: Arc<AtomicU32>,
    }

    impl aioduct::Middleware for StreamingRequestMiddleware {
        fn on_request(
            &self,
            req: &mut http::Request<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.on_request_count.fetch_add(1, Ordering::SeqCst);
            req.headers_mut().insert(
                http::header::HeaderName::from_static("x-streaming-header"),
                http::header::HeaderValue::from_static("streaming-injected"),
            );
        }
    }

    let on_request_count = Arc::new(AtomicU32::new(0));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(StreamingRequestMiddleware {
            on_request_count: on_request_count.clone(),
        })
        .build()
        .unwrap();

    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("streaming-payload"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();

    assert_eq!(
        on_request_count.load(Ordering::SeqCst),
        1,
        "on_request should fire for streaming request"
    );
    assert!(
        body.contains("header=streaming-injected"),
        "server should receive streaming header, got: {body}"
    );
    assert!(
        body.contains("body=streaming-payload"),
        "streaming body should be preserved through middleware, got: {body}"
    );
}
