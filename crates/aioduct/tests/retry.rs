#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body::Body as _;
use http_body_util::{BodyExt, Full};
use hyper::Response;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

#[tokio::test]
async fn test_retry_on_server_error() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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
async fn retry_uses_method_after_middleware() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_server = attempts.clone();
    let (addr, _counter) = h1_server_with(move |req| {
        let attempts = attempts_for_server.clone();
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.method(), http::Method::POST);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(500)
                    .body(Full::new(Bytes::from_static(b"not retried")))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |request: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                *request.method_mut() = http::Method::POST;
            },
        )
        .build()
        .unwrap();
    let response = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_retry_exhausted() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
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
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_retry_429_too_many_requests() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(429)
                        .header("retry-after", "0")
                        .body(Full::new(Bytes::from("rate limited")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("ok"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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
    assert_eq!(resp.text().await.unwrap(), "ok");
    assert_eq!(attempt.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn test_retry_with_budget_exhaustion() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let budget = aioduct::RetryBudget::new(1, 1);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(5)
                .initial_backoff(Duration::from_millis(10))
                .budget(budget),
        )
        .send()
        .await
        .unwrap();

    // Budget of 1 token: original request + 1 retry, then budget exhausted → returns 500
    assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_retry_with_timeout_succeeds_on_retry() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_millis(200))
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10)),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(attempt.load(Ordering::SeqCst) >= 2);
}
#[tokio::test]
async fn test_retry_budget_deposit_on_success() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::from("error")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("ok"))))
            }
        }
    })
    .await;

    let budget = aioduct::RetryBudget::new(5, 2);
    assert_eq!(budget.available(), 5);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10))
                .budget(budget.clone()),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    // Budget started at 5, one retry withdrew 1 (→4), success deposited 2 (→5, capped at max)
    assert_eq!(budget.available(), 5);
}
#[tokio::test]
async fn test_retry_exhaustion_returns_last_error() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10)),
        )
        .send()
        .await;

    if let Ok(r) = resp {
        assert!(r.status().is_server_error());
    }
    assert_eq!(attempt.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn test_retry_with_retry_after_header() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(429)
                        .header("retry-after", "0")
                        .body(Full::new(Bytes::from("rate limited")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("ok"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn retryable_final_response_replays_finalized_request() {
    let target_attempts = Arc::new(AtomicU32::new(0));
    let target_attempts_clone = target_attempts.clone();
    let (target_addr, _target_counter) = h1_server_with(move |_req| {
        let target_attempts = target_attempts_clone.clone();
        async move {
            let n = target_attempts.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::from("retry later")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("ok"))))
            }
        }
    })
    .await;

    let origin_attempts = Arc::new(AtomicU32::new(0));
    let origin_attempts_clone = origin_attempts.clone();
    let (origin_addr, _origin_counter) = h1_server_with(move |req| {
        let origin_attempts = origin_attempts_clone.clone();
        let location = format!("http://{target_addr}/final");
        async move {
            let attempt = origin_attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                assert!(req.headers().get(http::header::COOKIE).is_none());
            } else {
                assert_eq!(
                    req.headers().get(http::header::COOKIE).unwrap(),
                    "redirect_chain=fresh"
                );
            }
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", location)
                    .header("set-cookie", "redirect_chain=fresh; Path=/")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(aioduct::CookieJar::new())
        .build()
        .unwrap();
    let resp = client
        .get(&format!("http://{origin_addr}/start"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::from_millis(0)),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "ok");
    assert_eq!(origin_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(target_attempts.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_retry_on_status_disabled_no_retry() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .retry_on_status(false),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(attempt.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn test_client_default_retry_with_recovery() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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
                Ok(Response::new(Full::new(Bytes::from("ok"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
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
    assert!(attempt.load(Ordering::SeqCst) >= 3);
}

// ── Custom retry classifier ────────────────────────────────────────────────

use aioduct::{RetryDecision, RetryOutcome};

struct DelegatingBody {
    inner: aioduct::body::RequestBodySend,
}

impl http_body::Body for DelegatingBody {
    type Data = Bytes;
    type Error = aioduct::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[tokio::test]
async fn byte_identical_middleware_wrapped_body_is_replayed_by_configured_retry() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let (addr, _counter) = h1_server_with(move |req| {
        let attempts = server_attempts.clone();
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                req.into_body().collect().await.unwrap().to_bytes(),
                "payload"
            );
            let status = if attempts.load(Ordering::SeqCst) == 1 {
                http::StatusCode::INTERNAL_SERVER_ERROR
            } else {
                http::StatusCode::OK
            };
            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |request: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                let placeholder = Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync();
                let inner = std::mem::replace(request.body_mut(), placeholder);
                *request.body_mut() = DelegatingBody { inner }.boxed_unsync();
            },
        )
        .build()
        .unwrap();
    let response = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("payload")
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO)
                .classify(|_| RetryDecision::Retry),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn middleware_polled_body_is_not_replayed_by_configured_retry() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let (addr, _counter) = h1_server_with(move |_req| {
        let attempts = server_attempts.clone();
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(500)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(
            |request: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                let waker = std::task::Waker::noop();
                let mut context = std::task::Context::from_waker(waker);
                let _ = std::pin::Pin::new(request.body_mut()).poll_frame(&mut context);
            },
        )
        .build()
        .unwrap();
    let response = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("payload")
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::ZERO)
                .classify(|_| RetryDecision::Retry),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

/// A classifier can force a retry on a status the built-in rules treat as
/// final (404), turning a normally non-retried response into a retried one.
#[tokio::test]
async fn classifier_forces_retry_on_non_default_status() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(404)
                        .body(Full::new(Bytes::from("not found")))
                        .unwrap(),
                )
            } else {
                Ok(Response::new(Full::new(Bytes::from("success"))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(10))
                .classify(|ctx| match ctx.outcome() {
                    RetryOutcome::Status(s) if s.as_u16() == 404 => RetryDecision::Retry,
                    _ => RetryDecision::UseDefault,
                }),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "success");
    assert_eq!(attempt.load(Ordering::SeqCst), 3);
}

/// A classifier can suppress a retry the built-in rules would perform (503),
/// returning the error response to the caller after a single attempt.
#[tokio::test]
async fn classifier_suppresses_default_retry() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(5)
                .initial_backoff(Duration::from_millis(10))
                .classify(|ctx| match ctx.outcome() {
                    RetryOutcome::Status(s) if s.as_u16() == 503 => RetryDecision::DoNotRetry,
                    _ => RetryDecision::UseDefault,
                }),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        attempt.load(Ordering::SeqCst),
        1,
        "classifier DoNotRetry should stop after the first attempt"
    );
}

/// A classifier opt-in retries a non-idempotent POST, which the built-in
/// idempotency rule would never retry on its own.
#[tokio::test]
async fn classifier_retries_non_idempotent_post() {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body("payload")
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(10))
                .classify(|ctx| match ctx.outcome() {
                    RetryOutcome::Status(s) if s.is_server_error() => RetryDecision::Retry,
                    _ => RetryDecision::UseDefault,
                }),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "ok");
    assert_eq!(
        attempt.load(Ordering::SeqCst),
        2,
        "explicit classifier opt-in should retry the POST once"
    );
}

/// UseDefault on every outcome leaves the built-in behavior unchanged: a 500
/// on a GET is still retried.
#[tokio::test]
async fn classifier_use_default_preserves_builtin_behavior() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(10))
                .classify(|_ctx| RetryDecision::UseDefault),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(attempt.load(Ordering::SeqCst), 3);
}

/// A classifier still respects max_retries: forcing Retry on every attempt
/// stops once the cap is reached and returns the last response.
#[tokio::test]
async fn classifier_retry_bounded_by_max_retries() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(404)
                    .body(Full::new(Bytes::from("nope")))
                    .unwrap(),
            )
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(10))
                .classify(|_ctx| RetryDecision::Retry),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    // 1 initial + 2 retries = 3 attempts.
    assert_eq!(attempt.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn streaming_body_status_is_not_retried_as_empty() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = Arc::clone(&attempts);

    let (addr, _counter) = h1_server_with(move |req| {
        let attempts = Arc::clone(&attempts_clone);
        async move {
            let body = req.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(body, Bytes::from_static(b"streaming payload"));
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .status(503)
                    .body(Full::new(Bytes::from_static(b"retry later")))
                    .unwrap(),
            )
        }
    })
    .await;

    let body = Full::new(Bytes::from_static(b"streaming payload"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(body)
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn consumed_streaming_body_error_is_not_retried_as_empty() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = Arc::clone(&attempts);

    let server = tokio::spawn(async move {
        while let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(250), listener.accept()).await
        {
            attempts_clone.fetch_add(1, Ordering::SeqCst);
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            let (header_end, content_length) = loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
                let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let header_end = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                break (header_end, content_length);
            };

            while request.len() < header_end + content_length {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            assert_eq!(
                &request[header_end..header_end + content_length],
                b"streaming payload"
            );
            drop(stream);
        }
    });

    let body = Full::new(Bytes::from_static(b"streaming payload"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(body)
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::ZERO),
        )
        .send()
        .await;

    assert!(result.is_err());
    server.await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
