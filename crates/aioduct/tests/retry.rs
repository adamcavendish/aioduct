#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

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
