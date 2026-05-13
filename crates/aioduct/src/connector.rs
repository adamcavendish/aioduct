use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Uri;
use tower_layer::Layer;
use tower_service::Service;

use crate::runtime::ConnectorSend;

/// Type-erased tower connector slot that can be stored without trait bounds on the struct.
///
/// Wraps a `LayeredConnector<C>` but erases the `C` type parameter so the parent
/// struct doesn't need `C: ConnectorSend` in its definition.
#[derive(Clone)]
pub(crate) struct TowerConnectorSlot {
    inner: Arc<dyn std::any::Any + Send + Sync>,
}

impl TowerConnectorSlot {
    pub(crate) fn new<C: ConnectorSend>(connector: LayeredConnector<C>) -> Self {
        Self {
            inner: Arc::new(connector),
        }
    }

    pub(crate) fn get<C: ConnectorSend>(&self) -> &LayeredConnector<C> {
        self.inner
            .downcast_ref::<LayeredConnector<C>>()
            .expect("TowerConnectorSlot type mismatch")
    }
}

/// A connector request containing the target address info.
#[derive(Debug, Clone)]
pub struct ConnectInfo {
    /// The target URI being connected to.
    pub uri: Uri,
    /// The resolved socket address.
    pub addr: SocketAddr,
}

/// Default connector that delegates to a [`ConnectorSend`] instance's `connect` method.
pub struct ConnectorService<C: ConnectorSend> {
    connector: C,
}

impl<C: ConnectorSend> ConnectorService<C> {
    /// Create a new connector service wrapping the given connector.
    pub fn new(connector: C) -> Self {
        Self { connector }
    }
}

impl<C: ConnectorSend + Default> Default for ConnectorService<C> {
    fn default() -> Self {
        Self {
            connector: C::default(),
        }
    }
}

impl<C: ConnectorSend> Clone for ConnectorService<C> {
    fn clone(&self) -> Self {
        Self {
            connector: self.connector.clone(),
        }
    }
}

impl<C: ConnectorSend> Service<ConnectInfo> for ConnectorService<C> {
    type Response = C::Stream;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<C::Stream>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, info: ConnectInfo) -> Self::Future {
        let connector = self.connector.clone();
        Box::pin(async move { connector.connect(info.addr).await })
    }
}

pub(crate) trait BoxedConnectorTrait<Stream>: Send + Sync {
    fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<Stream>> + Send>>;
}

struct ServiceConnector<S> {
    inner: std::sync::Mutex<S>,
}

impl<Stream, S> BoxedConnectorTrait<Stream> for ServiceConnector<S>
where
    Stream: 'static,
    S: Service<ConnectInfo, Response = Stream, Error = io::Error> + Send + Sync + Clone + 'static,
    S::Future: Send + 'static,
{
    fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<Stream>> + Send>> {
        let mut svc = self.inner.lock().unwrap().clone();
        Box::pin(async move {
            std::future::poll_fn(|cx| svc.poll_ready(cx)).await?;
            svc.call(info).await
        })
    }
}

/// A connector wrapped with tower layers.
pub(crate) struct LayeredConnector<C: ConnectorSend> {
    inner: Arc<dyn BoxedConnectorTrait<C::Stream>>,
}

impl<C: ConnectorSend> Clone for LayeredConnector<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<C: ConnectorSend> LayeredConnector<C> {
    pub fn new<S>(service: S) -> Self
    where
        S: Service<ConnectInfo, Response = C::Stream, Error = io::Error>
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
    ) -> Pin<Box<dyn Future<Output = io::Result<C::Stream>> + Send>> {
        self.inner.connect(info)
    }
}

/// Apply a tower layer to a connector service, producing a layered connector.
pub(crate) fn apply_layer<C, L>(connector: C, layer: L) -> LayeredConnector<C>
where
    C: ConnectorSend,
    L: Layer<ConnectorService<C>>,
    L::Service: Service<ConnectInfo, Response = C::Stream, Error = io::Error>
        + Send
        + Sync
        + Clone
        + 'static,
    <L::Service as Service<ConnectInfo>>::Future: Send + 'static,
{
    let base = ConnectorService::new(connector);
    let layered = layer.layer(base);
    LayeredConnector::new(layered)
}

#[cfg(all(test, feature = "tower", feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::tokio_rt::TcpConnector;

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
    fn connector_service_clone() {
        let conn = ConnectorService::new(TcpConnector);
        let _cloned = conn.clone();
    }

    #[test]
    fn connector_service_poll_ready() {
        let mut conn = ConnectorService::new(TcpConnector);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let result = Service::poll_ready(&mut conn, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[tokio::test]
    async fn apply_identity_layer() {
        let layer = tower_layer::Identity::new();
        let _layered: LayeredConnector<TcpConnector> = apply_layer(TcpConnector, layer);
    }

    #[tokio::test]
    async fn layered_connector_clone() {
        let layer = tower_layer::Identity::new();
        let layered: LayeredConnector<TcpConnector> = apply_layer(TcpConnector, layer);
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
        let connector: LayeredConnector<TcpConnector> = apply_layer(TcpConnector, layer);
        let info = ConnectInfo {
            uri: format!("http://{addr}").parse().unwrap(),
            addr,
        };
        let stream = connector.connect(info).await.unwrap();
        drop(stream);
    }

    #[tokio::test]
    async fn connector_service_call_connects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let mut conn = ConnectorService::new(TcpConnector);
        let info = ConnectInfo {
            uri: format!("http://{addr}").parse().unwrap(),
            addr,
        };
        let stream = conn.call(info).await.unwrap();
        drop(stream);
    }
}
