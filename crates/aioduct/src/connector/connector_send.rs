use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tower_service::Service;

use crate::runtime::ConnectorSend;

use super::ConnectInfo;

/// Type-erased tower connector slot that can be stored without trait bounds on the struct.
///
/// Wraps a `LayeredConnectorSend<C>` but erases the `C` type parameter so the parent
/// struct doesn't need `C: ConnectorSend` in its definition.
#[derive(Clone)]
pub(crate) struct TowerConnectorSendSlot {
    inner: Arc<dyn std::any::Any + Send + Sync>,
}

impl TowerConnectorSendSlot {
    pub(crate) fn new<C: ConnectorSend>(connector: LayeredConnectorSend<C>) -> Self {
        Self {
            inner: Arc::new(connector),
        }
    }

    pub(crate) fn get<C: ConnectorSend>(&self) -> &LayeredConnectorSend<C> {
        #[allow(clippy::expect_used)]
        self.inner
            .downcast_ref::<LayeredConnectorSend<C>>()
            .expect("TowerConnectorSendSlot type mismatch")
    }
}

/// Default connector that delegates to a [`ConnectorSend`] instance's `connect` method.
pub struct ConnectorServiceSend<C: ConnectorSend> {
    connector: C,
}

impl<C: ConnectorSend> ConnectorServiceSend<C> {
    /// Create a new connector service wrapping the given connector.
    pub fn new(connector: C) -> Self {
        Self { connector }
    }
}

impl<C: ConnectorSend + Default> Default for ConnectorServiceSend<C> {
    fn default() -> Self {
        Self {
            connector: C::default(),
        }
    }
}

impl<C: ConnectorSend> Clone for ConnectorServiceSend<C> {
    fn clone(&self) -> Self {
        Self {
            connector: self.connector.clone(),
        }
    }
}

impl<C: ConnectorSend> Service<ConnectInfo> for ConnectorServiceSend<C> {
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

pub(crate) trait BoxedConnectorSendTrait<Stream>: Send + Sync {
    fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<Stream>> + Send>>;
}

struct ServiceConnectorSend<S> {
    inner: std::sync::Mutex<S>,
}

impl<Stream, S> BoxedConnectorSendTrait<Stream> for ServiceConnectorSend<S>
where
    Stream: 'static,
    S: Service<ConnectInfo, Response = Stream, Error = io::Error> + Send + Sync + Clone + 'static,
    S::Future: Send + 'static,
{
    fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<Stream>> + Send>> {
        let svc = match self.inner.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return Box::pin(async { Err(io::Error::other("lock poisoned")) }),
        };
        Box::pin(async move {
            let mut svc = svc;
            std::future::poll_fn(|cx| svc.poll_ready(cx)).await?;
            svc.call(info).await
        })
    }
}

/// A connector wrapped with tower layers.
pub(crate) struct LayeredConnectorSend<C: ConnectorSend> {
    inner: Arc<dyn BoxedConnectorSendTrait<C::Stream>>,
}

impl<C: ConnectorSend> Clone for LayeredConnectorSend<C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<C: ConnectorSend> LayeredConnectorSend<C> {
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
            inner: Arc::new(ServiceConnectorSend {
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
pub(crate) fn apply_layer_send<C, L>(connector: C, layer: L) -> LayeredConnectorSend<C>
where
    C: ConnectorSend,
    L: tower_layer::Layer<ConnectorServiceSend<C>>,
    L::Service: Service<ConnectInfo, Response = C::Stream, Error = io::Error>
        + Send
        + Sync
        + Clone
        + 'static,
    <L::Service as Service<ConnectInfo>>::Future: Send + 'static,
{
    let base = ConnectorServiceSend::new(connector);
    let layered = layer.layer(base);
    LayeredConnectorSend::new(layered)
}

#[cfg(all(test, feature = "tower", feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::tokio_rt::TcpConnector;
    use std::net::SocketAddr;

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
    fn connector_service_poll_ready() {
        let mut conn = ConnectorServiceSend::new(TcpConnector);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let result = Service::poll_ready(&mut conn, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn connector_service_default() {
        let conn = ConnectorServiceSend::<TcpConnector>::default();
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut conn = conn;
        let result = Service::poll_ready(&mut conn, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[tokio::test]
    async fn layered_connector_connect_failure() {
        let layer = tower_layer::Identity::new();
        let connector: LayeredConnectorSend<TcpConnector> = apply_layer_send(TcpConnector, layer);
        let info = ConnectInfo {
            uri: "http://127.0.0.1:1".parse().unwrap(),
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        let result = connector.connect(info).await;
        assert!(result.is_err());
    }
}
