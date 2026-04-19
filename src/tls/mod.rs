#[cfg(feature = "rustls")]
mod rustls_connector;
#[cfg(feature = "rustls")]
pub use rustls_connector::{AlpnProtocol, RustlsConnector, TlsStream};

use std::future::Future;
use std::io;
use std::pin::Pin;

use crate::runtime::Runtime;

/// Async TLS handshake abstraction.
pub trait TlsConnect<R: Runtime>: Send + Sync + 'static {
    type Stream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;

    fn connect(
        &self,
        server_name: &str,
        stream: R::TcpStream,
    ) -> Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + Send + '_>>;
}
