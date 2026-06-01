use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
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
    /// Shared active stream count for H2/H3 multiplex clones.
    active_streams: Option<Arc<AtomicUsize>>,
    /// Permit held by an active H2/H3 multiplex clone.
    _active_stream_permit: Option<ActiveStreamPermit>,
    /// Upgrade handle for Local path (!Send) HTTP/1.1 upgrades.
    pub(crate) upgrade_handle_local: Option<crate::upgrade::UpgradeHandleLocal>,
    /// Weak reference to the pool this connection was checked out from.
    pub(crate) pool: Weak<std::sync::Mutex<super::PoolInner<B>>>,
    /// The pool key this connection was checked out under.
    pub(crate) key: Option<super::PoolKey>,
}

struct ActiveStreamPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for ActiveStreamPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
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
            active_streams: None,
            _active_stream_permit: None,
            upgrade_handle_local: None,
            pool: Weak::new(),
            key: None,
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
            active_streams: Some(Arc::new(AtomicUsize::new(0))),
            _active_stream_permit: None,
            upgrade_handle_local: None,
            pool: Weak::new(),
            key: None,
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
            active_streams: Some(Arc::new(AtomicUsize::new(0))),
            _active_stream_permit: None,
            upgrade_handle_local: None,
            pool: Weak::new(),
            key: None,
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

    fn acquire_multiplex_permit(
        &self,
        max_active: Option<NonZeroUsize>,
    ) -> Option<ActiveStreamPermit> {
        let active = self.active_streams.as_ref()?.clone();

        if let Some(max_active) = max_active {
            let max = max_active.get();
            let mut current = active.load(Ordering::Acquire);
            loop {
                if current >= max {
                    return None;
                }
                match active.compare_exchange_weak(
                    current,
                    current + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return Some(ActiveStreamPermit { active }),
                    Err(observed) => current = observed,
                }
            }
        }

        active.fetch_add(1, Ordering::AcqRel);
        Some(ActiveStreamPermit { active })
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn active_multiplex_streams(&self) -> Option<usize> {
        self.active_streams
            .as_ref()
            .map(|active| active.load(Ordering::Acquire))
    }
}

impl<B> Drop for PooledConnection<B> {
    fn drop(&mut self) {
        if let Some(ref key) = self.key {
            if let Some(pool_inner) = self.pool.upgrade() {
                if let Ok(mut inner) = pool_inner.lock() {
                    if let Some(count) = inner.active.get_mut(key) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            inner.active.remove(key);
                        }
                    }
                }
            }
        }
    }
}

impl<B: 'static> PooledConnection<B> {
    /// Clone the underlying send handle for H2/H3 multiplexing.
    ///
    /// Returns `None` for H1 connections (no multiplexing).
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn clone_for_multiplex(&self) -> Option<Self> {
        self.clone_for_multiplex_with_limit(None)
    }

    /// Clone the underlying send handle for H2/H3 multiplexing if capacity allows.
    ///
    /// Returns `None` for H1 connections or when the configured active stream
    /// limit for this connection has already been reached.
    pub(crate) fn clone_for_multiplex_with_limit(
        &self,
        max_active: Option<NonZeroUsize>,
    ) -> Option<Self> {
        let active_stream_permit = self.acquire_multiplex_permit(max_active)?;
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
            active_streams: self.active_streams.clone(),
            _active_stream_permit: Some(active_stream_permit),
            upgrade_handle_local: None,
            pool: self.pool.clone(),
            key: self.key.clone(),
        })
    }
}
