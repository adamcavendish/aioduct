use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Uri;
use tower_layer::Layer;
use tower_service::Service;

use crate::runtime::Runtime;

/// A connector request containing the target address info.
#[derive(Debug, Clone)]
pub struct ConnectInfo {
    pub uri: Uri,
    pub addr: SocketAddr,
}

/// Default connector that delegates to the runtime's `connect` method.
pub struct RuntimeConnector<R: Runtime> {
    _runtime: std::marker::PhantomData<R>,
}

impl<R: Runtime> RuntimeConnector<R> {
    pub fn new() -> Self {
        Self {
            _runtime: std::marker::PhantomData,
        }
    }
}

impl<R: Runtime> Default for RuntimeConnector<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Runtime> Clone for RuntimeConnector<R> {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl<R: Runtime> Service<ConnectInfo> for RuntimeConnector<R> {
    type Response = R::TcpStream;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<R::TcpStream>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, info: ConnectInfo) -> Self::Future {
        Box::pin(async move { R::connect(info.addr).await })
    }
}

pub(crate) trait BoxedConnector<R: Runtime>: Send + Sync {
    fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<R::TcpStream>> + Send>>;
}

struct ServiceConnector<S> {
    inner: std::sync::Mutex<S>,
}

impl<R, S> BoxedConnector<R> for ServiceConnector<S>
where
    R: Runtime,
    S: Service<ConnectInfo, Response = R::TcpStream, Error = io::Error>
        + Send
        + Sync
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<R::TcpStream>> + Send>> {
        let mut svc = self.inner.lock().unwrap().clone();
        Box::pin(async move {
            std::future::poll_fn(|cx| svc.poll_ready(cx)).await?;
            svc.call(info).await
        })
    }
}

/// A connector wrapped with tower layers.
pub(crate) struct LayeredConnector<R: Runtime> {
    inner: Arc<dyn BoxedConnector<R>>,
}

impl<R: Runtime> Clone for LayeredConnector<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<R: Runtime> LayeredConnector<R> {
    pub fn new<S>(service: S) -> Self
    where
        S: Service<ConnectInfo, Response = R::TcpStream, Error = io::Error>
            + Send
            + Sync
            + Clone
            + 'static,
        S::Future: Send + 'static,
    {
        Self {
            inner: Arc::new(ServiceConnector {
                inner: std::sync::Mutex::new(service),
            }),
        }
    }

    pub fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<R::TcpStream>> + Send>> {
        self.inner.connect(info)
    }
}

/// Apply a tower layer to the default runtime connector, producing a layered connector.
pub(crate) fn apply_layer<R, L>(layer: L) -> LayeredConnector<R>
where
    R: Runtime,
    L: Layer<RuntimeConnector<R>>,
    L::Service: Service<ConnectInfo, Response = R::TcpStream, Error = io::Error>
        + Send
        + Sync
        + Clone
        + 'static,
    <L::Service as Service<ConnectInfo>>::Future: Send + 'static,
{
    let base = RuntimeConnector::<R>::new();
    let layered = layer.layer(base);
    LayeredConnector::new(layered)
}
