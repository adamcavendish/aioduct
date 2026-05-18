use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::clock::Instant;

/// An established HTTP connection at a specific protocol version.
pub(crate) enum HttpConnection<B> {
    /// An HTTP/1.1 connection.
    H1(hyper::client::conn::http1::SendRequest<B>),
    /// An HTTP/2 connection.
    H2(hyper::client::conn::http2::SendRequest<B>),
    /// An HTTP/3 connection.
    #[cfg(all(feature = "http3", feature = "rustls"))]
    H3(h3::client::SendRequest<h3_quinn::OpenStreams, bytes::Bytes>),
}

/// A pooled HTTP connection wrapper.
pub(crate) struct PooledConnection<B> {
    pub(crate) conn: HttpConnection<B>,
    pub(crate) remote_addr: Option<SocketAddr>,
    pub(crate) tls_info: Option<crate::tls::TlsInfo>,
    pub(crate) tls_handshake_duration: Option<Duration>,
    /// Subject Alternative Names from peer cert (for connection coalescing).
    pub(crate) sans: Arc<[String]>,
    /// When this connection was established.
    pub(crate) created_at: Instant,
    /// Number of request/response cycles served on this connection.
    pub(crate) requests_served: u32,
    /// Cumulative bytes sent (request bodies) on this connection.
    pub(crate) bytes_sent: u64,
    /// Cumulative bytes received (response bodies) on this connection.
    pub(crate) bytes_received: u64,
    /// True when this is a cloned handle for H2/H3 multiplexing.
    pub(crate) is_multiplex_clone: bool,
    /// Upgrade handle for Local path (!Send) HTTP/1.1 upgrades.
    pub(crate) upgrade_handle_local: Option<crate::upgrade::UpgradeHandleLocal>,
}

impl<B> PooledConnection<B> {
    /// Wrap an HTTP/1.1 connection.
    pub(crate) fn new_h1(sender: hyper::client::conn::http1::SendRequest<B>) -> Self {
        Self {
            conn: HttpConnection::H1(sender),
            remote_addr: None,
            tls_info: None,
            tls_handshake_duration: None,
            sans: Arc::from([]),
            created_at: Instant::now(),
            requests_served: 0,
            bytes_sent: 0,
            bytes_received: 0,
            is_multiplex_clone: false,
            upgrade_handle_local: None,
        }
    }

    /// Wrap an HTTP/2 connection.
    pub(crate) fn new_h2(sender: hyper::client::conn::http2::SendRequest<B>) -> Self {
        Self {
            conn: HttpConnection::H2(sender),
            remote_addr: None,
            tls_info: None,
            tls_handshake_duration: None,
            sans: Arc::from([]),
            created_at: Instant::now(),
            requests_served: 0,
            bytes_sent: 0,
            bytes_received: 0,
            is_multiplex_clone: false,
            upgrade_handle_local: None,
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
            sans: Arc::from([]),
            created_at: Instant::now(),
            requests_served: 0,
            bytes_sent: 0,
            bytes_received: 0,
            is_multiplex_clone: false,
            upgrade_handle_local: None,
        }
    }

    /// Returns true if the connection is ready to send a request.
    pub(crate) fn is_ready(&self) -> bool {
        match &self.conn {
            HttpConnection::H1(s) => s.is_ready(),
            HttpConnection::H2(s) => s.is_ready(),
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(s) => {
                use h3::ConnectionState as _;
                !s.is_closing() && s.get_conn_error().is_none()
            }
        }
    }

    /// Returns true if this is an HTTP/2 or HTTP/3 multiplexed connection.
    pub(crate) fn is_h2_or_h3(&self) -> bool {
        match &self.conn {
            HttpConnection::H1(_) => false,
            HttpConnection::H2(_) => true,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => true,
        }
    }
}

impl<B: 'static> PooledConnection<B> {
    /// Clone the underlying send handle for H2/H3 multiplexing.
    ///
    /// Returns `None` for H1 connections (no multiplexing).
    pub(crate) fn clone_for_multiplex(&self) -> Option<Self> {
        let conn = match &self.conn {
            HttpConnection::H1(_) => return None,
            HttpConnection::H2(s) => HttpConnection::H2(s.clone()),
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(s) => HttpConnection::H3(s.clone()),
        };
        Some(Self {
            conn,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info.clone(),
            tls_handshake_duration: self.tls_handshake_duration,
            sans: self.sans.clone(),
            created_at: self.created_at,
            requests_served: 0,
            bytes_sent: 0,
            bytes_received: 0,
            is_multiplex_clone: true,
            upgrade_handle_local: None,
        })
    }
}
