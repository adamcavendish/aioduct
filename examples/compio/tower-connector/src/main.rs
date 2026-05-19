use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use aioduct::CompioClient;

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
    S::Future: 'static,
    S::Response: 'static,
    Req: std::fmt::Debug,
{
    type Response = S::Response;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, std::io::Error>>>>;

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

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = CompioClient::builder()
            .connector_layer_local(LoggingLayer)
            .build_local()
            .unwrap();

        let resp = client.get_local("https://httpbin.org/get")?.send().await?;

        println!("Status: {}", resp.status());
        println!("Body:\n{}", resp.text().await?);

        Ok(())
    })
}
