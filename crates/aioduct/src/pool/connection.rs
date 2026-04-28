use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Duration;

use crate::runtime::Runtime;

/// An established HTTP connection at a specific protocol version.
pub(crate) enum HttpConnection {
    /// An HTTP/1.1 connection.
    H1(hyper::client::conn::http1::SendRequest<crate::error::AioductBody>),
    /// An HTTP/2 connection.
    H2(hyper::client::conn::http2::SendRequest<crate::error::AioductBody>),
    /// An HTTP/3 connection.
    #[cfg(all(feature = "http3", feature = "rustls"))]
    H3(h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>),
}

/// A pooled HTTP connection wrapper.
pub(crate) struct PooledConnection<R: Runtime> {
    pub(crate) conn: HttpConnection,
    pub(crate) remote_addr: Option<SocketAddr>,
    pub(crate) tls_info: Option<crate::tls::TlsInfo>,
    pub(crate) tls_handshake_duration: Option<Duration>,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> PooledConnection<R> {
    /// Wrap an HTTP/1.1 connection.
    pub(crate) fn new_h1(
        sender: hyper::client::conn::http1::SendRequest<crate::error::AioductBody>,
    ) -> Self {
        Self {
            conn: HttpConnection::H1(sender),
            remote_addr: None,
            tls_info: None,
            tls_handshake_duration: None,
            _runtime: PhantomData,
        }
    }

    /// Wrap an HTTP/2 connection.
    pub(crate) fn new_h2(
        sender: hyper::client::conn::http2::SendRequest<crate::error::AioductBody>,
    ) -> Self {
        Self {
            conn: HttpConnection::H2(sender),
            remote_addr: None,
            tls_info: None,
            tls_handshake_duration: None,
            _runtime: PhantomData,
        }
    }

    /// Wrap an HTTP/3 connection.
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) fn new_h3(
        sender: h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>,
    ) -> Self {
        Self {
            conn: HttpConnection::H3(sender),
            remote_addr: None,
            tls_info: None,
            tls_handshake_duration: None,
            _runtime: PhantomData,
        }
    }

    /// Returns true if the connection is ready to send a request.
    pub(crate) fn is_ready(&self) -> bool {
        match &self.conn {
            HttpConnection::H1(s) => s.is_ready(),
            HttpConnection::H2(s) => s.is_ready(),
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => true,
        }
    }
}
