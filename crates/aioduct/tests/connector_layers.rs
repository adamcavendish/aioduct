#![cfg(all(feature = "tokio", feature = "tower"))]

use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;

use aioduct_test_server::h1::h1_server;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use aioduct::connector::ConnectInfo;
use aioduct::runtime::tokio_rt::TcpConnector;
use tower_layer::Layer;
use tower_service::Service;

#[derive(Clone)]
struct IdentityLayer;

impl<S> Layer<S> for IdentityLayer {
    type Service = S;
    fn layer(&self, inner: S) -> S {
        inner
    }
}

#[derive(Clone)]
struct DelayService<S> {
    inner: S,
    delay: Duration,
}

impl<S> Service<ConnectInfo> for DelayService<S>
where
    S: Service<ConnectInfo> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: ConnectInfo) -> Self::Future {
        let mut inner = self.inner.clone();
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            inner.call(req).await
        })
    }
}

#[derive(Clone)]
struct TimeoutLayer {
    timeout: Duration,
}

impl TimeoutLayer {
    fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl<S: Clone> Layer<S> for TimeoutLayer {
    type Service = TimeoutService<S>;
    fn layer(&self, inner: S) -> TimeoutService<S> {
        TimeoutService {
            inner,
            timeout: self.timeout,
        }
    }
}

#[derive(Clone)]
struct TimeoutService<S> {
    inner: S,
    timeout: Duration,
}

impl<S> Service<ConnectInfo> for TimeoutService<S>
where
    S: Service<ConnectInfo, Error = std::io::Error> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
{
    type Response = S::Response;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, std::io::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: ConnectInfo) -> Self::Future {
        let mut inner = self.inner.clone();
        let timeout = self.timeout;
        Box::pin(async move {
            match tokio::time::timeout(timeout, inner.call(req)).await {
                Ok(result) => result,
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connection timed out",
                )),
            }
        })
    }
}

#[tokio::test]
async fn identity_layer() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connector_layer(IdentityLayer)
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
async fn timeout_layer_fast_connect() {
    let (addr, _counter) = h1_server().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connector_layer(TimeoutLayer::new(Duration::from_secs(5)))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[tokio::test]
async fn timeout_layer_slow_connect() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connector_layer(TimeoutLayer::new(Duration::from_millis(100)))
        .build();

    let result = client
        .get("http://192.0.2.1:81/slow")
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn delay_layer_with_timeout_layer_triggers() {
    let (addr, _counter) = h1_server().await;

    // Since connector_layer replaces the previous layer, we compose
    // delay + timeout into a single layer using a wrapper.
    #[derive(Clone)]
    struct DelayThenTimeoutLayer {
        delay: Duration,
        timeout: Duration,
    }

    impl<S: Clone> Layer<S> for DelayThenTimeoutLayer {
        type Service = TimeoutService<DelayService<S>>;
        fn layer(&self, inner: S) -> Self::Service {
            let delayed = DelayService {
                inner,
                delay: self.delay,
            };
            TimeoutService {
                inner: delayed,
                timeout: self.timeout,
            }
        }
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connector_layer(DelayThenTimeoutLayer {
            delay: Duration::from_millis(200),
            timeout: Duration::from_millis(100),
        })
        .build();

    let result = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    assert!(result.is_err(), "delay exceeds timeout layer, should fail");
}

#[tokio::test]
async fn delay_layer_within_timeout_succeeds() {
    let (addr, _counter) = h1_server().await;

    #[derive(Clone)]
    struct DelayThenTimeoutLayer {
        delay: Duration,
        timeout: Duration,
    }

    impl<S: Clone> Layer<S> for DelayThenTimeoutLayer {
        type Service = TimeoutService<DelayService<S>>;
        fn layer(&self, inner: S) -> Self::Service {
            let delayed = DelayService {
                inner,
                delay: self.delay,
            };
            TimeoutService {
                inner: delayed,
                timeout: self.timeout,
            }
        }
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .connector_layer(DelayThenTimeoutLayer {
            delay: Duration::from_millis(50),
            timeout: Duration::from_millis(500),
        })
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
