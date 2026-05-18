use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tower_service::Service;

use crate::runtime::ConnectorLocal;

use super::ConnectInfo;

/// Type-erased local tower connector slot (`!Send`).
pub(crate) struct TowerConnectorLocalSlot {
    inner: std::rc::Rc<dyn std::any::Any>,
}

impl TowerConnectorLocalSlot {
    pub(crate) fn new<C: ConnectorLocal + Clone>(connector: LayeredConnectorLocal<C>) -> Self {
        Self {
            inner: std::rc::Rc::new(connector),
        }
    }

    pub(crate) fn get<C: ConnectorLocal + Clone>(&self) -> &LayeredConnectorLocal<C> {
        #[allow(clippy::expect_used)]
        self.inner
            .downcast_ref::<LayeredConnectorLocal<C>>()
            .expect("TowerConnectorLocalSlot type mismatch")
    }
}

impl Clone for TowerConnectorLocalSlot {
    fn clone(&self) -> Self {
        Self {
            inner: std::rc::Rc::clone(&self.inner),
        }
    }
}

/// Default connector service for the `!Send` local path.
pub struct ConnectorServiceLocal<C: ConnectorLocal + Clone> {
    connector: C,
}

impl<C: ConnectorLocal + Clone> ConnectorServiceLocal<C> {
    /// Create a new local connector service wrapping the given connector.
    pub fn new(connector: C) -> Self {
        Self { connector }
    }
}

impl<C: ConnectorLocal + Clone + Default> Default for ConnectorServiceLocal<C> {
    fn default() -> Self {
        Self {
            connector: C::default(),
        }
    }
}

impl<C: ConnectorLocal + Clone> Clone for ConnectorServiceLocal<C> {
    fn clone(&self) -> Self {
        Self {
            connector: self.connector.clone(),
        }
    }
}

impl<C: ConnectorLocal + Clone> Service<ConnectInfo> for ConnectorServiceLocal<C> {
    type Response = C::Stream;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<C::Stream>>>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, info: ConnectInfo) -> Self::Future {
        let connector = self.connector.clone();
        Box::pin(async move { connector.connect(info.addr).await })
    }
}

pub(crate) trait BoxedConnectorLocalTrait<Stream> {
    fn connect(&self, info: ConnectInfo) -> Pin<Box<dyn Future<Output = io::Result<Stream>>>>;
}

struct ServiceConnectorLocal<S> {
    inner: std::cell::RefCell<S>,
}

impl<Stream, S> BoxedConnectorLocalTrait<Stream> for ServiceConnectorLocal<S>
where
    Stream: 'static,
    S: Service<ConnectInfo, Response = Stream, Error = io::Error> + Clone + 'static,
{
    fn connect(&self, info: ConnectInfo) -> Pin<Box<dyn Future<Output = io::Result<Stream>>>> {
        let svc = self.inner.borrow().clone();
        Box::pin(async move {
            let mut svc = svc;
            std::future::poll_fn(|cx| svc.poll_ready(cx)).await?;
            svc.call(info).await
        })
    }
}

/// A local (`!Send`) connector wrapped with tower layers.
pub(crate) struct LayeredConnectorLocal<C: ConnectorLocal + Clone> {
    inner: std::rc::Rc<dyn BoxedConnectorLocalTrait<C::Stream>>,
}

impl<C: ConnectorLocal + Clone> Clone for LayeredConnectorLocal<C> {
    fn clone(&self) -> Self {
        Self {
            inner: std::rc::Rc::clone(&self.inner),
        }
    }
}

impl<C: ConnectorLocal + Clone> LayeredConnectorLocal<C> {
    pub fn new<S>(service: S) -> Self
    where
        S: Service<ConnectInfo, Response = C::Stream, Error = io::Error> + Clone + 'static,
    {
        Self {
            inner: std::rc::Rc::new(ServiceConnectorLocal {
                inner: std::cell::RefCell::new(service),
            }),
        }
    }

    pub fn connect(
        &self,
        info: ConnectInfo,
    ) -> Pin<Box<dyn Future<Output = io::Result<C::Stream>>>> {
        self.inner.connect(info)
    }
}

/// Apply a tower layer to a local connector service, producing a layered connector.
pub(crate) fn apply_layer_local<C, L>(connector: C, layer: L) -> LayeredConnectorLocal<C>
where
    C: ConnectorLocal + Clone,
    L: tower_layer::Layer<ConnectorServiceLocal<C>>,
    L::Service: Service<ConnectInfo, Response = C::Stream, Error = io::Error> + Clone + 'static,
{
    let base = ConnectorServiceLocal::new(connector);
    let layered = layer.layer(base);
    LayeredConnectorLocal::new(layered)
}

#[cfg(all(test, feature = "tower", feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::tokio_rt::TcpConnector;

    #[test]
    fn connector_service_local_poll_ready() {
        let mut conn = ConnectorServiceLocal::new(TcpConnector);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let result = Service::poll_ready(&mut conn, &mut cx);
        assert!(matches!(result, Poll::Ready(Ok(()))));
    }

    #[test]
    fn connector_service_local_clone() {
        let conn = ConnectorServiceLocal::new(TcpConnector);
        let cloned = conn.clone();
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut cloned = cloned;
        assert!(matches!(
            Service::poll_ready(&mut cloned, &mut cx),
            Poll::Ready(Ok(()))
        ));
    }

    #[test]
    fn connector_service_local_default() {
        let conn = ConnectorServiceLocal::<TcpConnector>::default();
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut conn = conn;
        assert!(matches!(
            Service::poll_ready(&mut conn, &mut cx),
            Poll::Ready(Ok(()))
        ));
    }

    #[tokio::test]
    async fn layered_connector_local_connect_failure() {
        let layer = tower_layer::Identity::new();
        let connector = apply_layer_local(TcpConnector, layer);
        let info = ConnectInfo {
            uri: "http://127.0.0.1:1".parse().unwrap(),
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        let result = connector.connect(info).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn layered_connector_local_clone_shares_behavior() {
        let layer = tower_layer::Identity::new();
        let connector = apply_layer_local(TcpConnector, layer);
        let cloned = connector.clone();
        let info = ConnectInfo {
            uri: "http://127.0.0.1:1".parse().unwrap(),
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        let result = cloned.connect(info).await;
        assert!(result.is_err());
    }

    #[test]
    fn tower_connector_local_slot_roundtrip() {
        let layer = tower_layer::Identity::new();
        let connector = apply_layer_local(TcpConnector, layer);
        let slot = TowerConnectorLocalSlot::new(connector);
        let retrieved = slot.get::<TcpConnector>();
        let info = ConnectInfo {
            uri: "http://127.0.0.1:1".parse().unwrap(),
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        // Verify the slot round-trips correctly by calling connect
        let _fut = retrieved.connect(info);
    }

    #[test]
    fn tower_connector_local_slot_clone() {
        let layer = tower_layer::Identity::new();
        let connector = apply_layer_local(TcpConnector, layer);
        let slot = TowerConnectorLocalSlot::new(connector);
        let cloned = slot.clone();
        // Both the original and clone should resolve to the same type
        let _retrieved = cloned.get::<TcpConnector>();
    }
}
