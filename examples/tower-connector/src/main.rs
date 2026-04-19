use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use aioduct::Client;
use aioduct::runtime::TokioRuntime;

/// A simple tower Layer that logs connection attempts.
/// This demonstrates how to wrap the TCP connector with custom logic.
#[derive(Clone)]
struct LoggingLayer;

impl<S> tower_layer::Layer<S> for LoggingLayer {
    type Service = LoggingConnector<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LoggingConnector { inner }
    }
}

/// The service produced by LoggingLayer.
#[derive(Clone)]
struct LoggingConnector<S> {
    inner: S,
}

impl<S, Req> tower_service::Service<Req> for LoggingConnector<S>
where
    S: tower_service::Service<Req, Error = std::io::Error>,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
    Req: std::fmt::Debug,
{
    type Response = S::Response;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, std::io::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        println!("[connector] connecting to {:?}", req);
        let fut = self.inner.call(req);
        Box::pin(async move {
            let result = fut.await;
            match &result {
                Ok(_) => println!("[connector] connected successfully"),
                Err(e) => println!("[connector] connection failed: {e}"),
            }
            result
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // Tower connector layer wraps the underlying TCP/TLS connector.
    // This is useful for adding logging, metrics, or custom
    // logic at the connection establishment level.

    let client = Client::<TokioRuntime>::builder()
        .connector_layer(LoggingLayer)
        .build();

    let resp = client.get("https://httpbin.org/get")?.send().await?;

    println!("Status: {}", resp.status());
    println!("Body:\n{}", resp.text().await?);

    Ok(())
}
