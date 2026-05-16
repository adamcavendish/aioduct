mod connector_local;
mod connector_send;

use std::net::SocketAddr;

use http::Uri;

pub use connector_local::ConnectorServiceLocal;
pub(crate) use connector_local::{TowerConnectorLocalSlot, apply_layer_local};

pub use connector_send::ConnectorServiceSend;
pub(crate) use connector_send::{TowerConnectorSendSlot, apply_layer_send};

/// A connector request containing the target address info.
#[derive(Debug, Clone)]
pub struct ConnectInfo {
    /// The target URI being connected to.
    pub uri: Uri,
    /// The resolved socket address.
    pub addr: SocketAddr,
}
