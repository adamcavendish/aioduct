use std::marker::PhantomData;
use std::net::SocketAddr;

use crate::runtime::Runtime;

/// An established HTTP connection at a specific protocol version.
pub enum HttpConnection {
    H1(hyper::client::conn::http1::SendRequest<crate::error::HyperBody>),
    H2(hyper::client::conn::http2::SendRequest<crate::error::HyperBody>),
    #[cfg(feature = "http3")]
    H3(h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>),
}

/// A pooled HTTP connection wrapper.
pub struct PooledConnection<R: Runtime> {
    pub(crate) conn: HttpConnection,
    pub(crate) remote_addr: Option<SocketAddr>,
    pub(crate) tls_info: Option<crate::tls::TlsInfo>,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> PooledConnection<R> {
    /// Wrap an HTTP/1.1 connection.
    pub fn new_h1(
        sender: hyper::client::conn::http1::SendRequest<crate::error::HyperBody>,
    ) -> Self {
        Self {
            conn: HttpConnection::H1(sender),
            remote_addr: None,
            tls_info: None,
            _runtime: PhantomData,
        }
    }

    /// Wrap an HTTP/2 connection.
    pub fn new_h2(
        sender: hyper::client::conn::http2::SendRequest<crate::error::HyperBody>,
    ) -> Self {
        Self {
            conn: HttpConnection::H2(sender),
            remote_addr: None,
            tls_info: None,
            _runtime: PhantomData,
        }
    }

    /// Wrap an HTTP/3 connection.
    #[cfg(feature = "http3")]
    pub fn new_h3(sender: h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>) -> Self {
        Self {
            conn: HttpConnection::H3(sender),
            remote_addr: None,
            tls_info: None,
            _runtime: PhantomData,
        }
    }

    /// Set the remote address of this connection.
    pub fn with_remote_addr(mut self, addr: SocketAddr) -> Self {
        self.remote_addr = Some(addr);
        self
    }

    /// Returns true if the connection is ready to send a request.
    pub fn is_ready(&self) -> bool {
        match &self.conn {
            HttpConnection::H1(s) => s.is_ready(),
            HttpConnection::H2(s) => s.is_ready(),
            #[cfg(feature = "http3")]
            HttpConnection::H3(_) => true,
        }
    }
}
