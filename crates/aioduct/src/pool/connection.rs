use std::marker::PhantomData;
use std::net::SocketAddr;
use std::task::{Context, Poll};
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
    /// Subject Alternative Names from peer cert (for connection coalescing).
    pub(crate) sans: Vec<String>,
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
            sans: Vec::new(),
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
            sans: Vec::new(),
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
            sans: Vec::new(),
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

    /// Returns true if this is an HTTP/1.1 connection.
    pub(crate) fn is_h1(&self) -> bool {
        matches!(&self.conn, HttpConnection::H1(_))
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

    /// Poll the H1 sender for readiness. Returns `Poll::Ready(true)` when
    /// the connection is ready for a new request, or `Poll::Ready(false)` if
    /// the connection has been closed/errored. For H2/H3 this always returns
    /// `Poll::Ready(true)` immediately.
    pub(crate) fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<bool> {
        match &mut self.conn {
            HttpConnection::H1(s) => match s.poll_ready(cx) {
                Poll::Ready(Ok(())) => Poll::Ready(true),
                Poll::Ready(Err(_)) => Poll::Ready(false),
                Poll::Pending => Poll::Pending,
            },
            HttpConnection::H2(s) => {
                let _ = s;
                Poll::Ready(true)
            }
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => Poll::Ready(true),
        }
    }
}
