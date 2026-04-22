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
    /// The target URI being connected to.
    pub uri: Uri,
    /// The resolved socket address.
    pub addr: SocketAddr,
}

/// Default connector that delegates to the runtime's `connect` method.
pub struct RuntimeConnector<R: Runtime> {
    _runtime: std::marker::PhantomData<R>,
}

impl<R: Runtime> RuntimeConnector<R> {
    /// Create a new runtime connector.
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

#[cfg(all(test, feature = "tower", feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::TokioRuntime;

    #[test]
    fn connect_info_debug_and_clone() {
        let info = ConnectInfo {
            uri: "http://example.com".parse().unwrap(),
            addr: "127.0.0.1:80".parse().unwrap(),
        };
        let dbg = format!("{info:?}");
        assert!(dbg.contains("ConnectInfo"));
        let cloned = info.clone();
        assert_eq!(cloned.addr, "127.0.0.1:80".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn runtime_connector_new_default_clone() {
        let conn: RuntimeConnector<TokioRuntime> = RuntimeConnector::new();
        let _default: RuntimeConnector<TokioRuntime> = Default::default();
        let _cloned = conn.clone();
    }

    #[test]
    fn runtime_connector_poll_ready() {
        let mut conn: RuntimeConnector<TokioRuntime> = RuntimeConnector::new();
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let result = Service::poll_ready(&mut conn, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[tokio::test]
    async fn apply_identity_layer() {
        let layer = tower_layer::Identity::new();
        let _layered: LayeredConnector<TokioRuntime> = apply_layer(layer);
    }

    #[tokio::test]
    async fn layered_connector_clone() {
        let layer = tower_layer::Identity::new();
        let layered: LayeredConnector<TokioRuntime> = apply_layer(layer);
        let _cloned = layered.clone();
    }

    #[tokio::test]
    async fn layered_connector_connects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let layer = tower_layer::Identity::new();
        let connector: LayeredConnector<TokioRuntime> = apply_layer(layer);
        let info = ConnectInfo {
            uri: format!("http://{addr}").parse().unwrap(),
            addr,
        };
        let stream = connector.connect(info).await.unwrap();
        drop(stream);
    }

    #[tokio::test]
    async fn runtime_connector_call_connects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let mut conn: RuntimeConnector<TokioRuntime> = RuntimeConnector::new();
        let info = ConnectInfo {
            uri: format!("http://{addr}").parse().unwrap(),
            addr,
        };
        let stream = conn.call(info).await.unwrap();
        drop(stream);
    }

    #[tokio::test]
    async fn builder_connector_layer_identity() {
        use crate::Client;
        let layer = tower_layer::Identity::new();
        let client = Client::<TokioRuntime>::builder()
            .connector_layer(layer)
            .build();
        assert!(client.connector.is_some());
    }
}
