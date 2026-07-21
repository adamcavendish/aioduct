use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use crate::clock::Instant;

/// Per-transport cumulative metrics shared across H2/H3 multiplex clones.
///
/// For H2/H3 connections, every `clone_for_multiplex_with_limit` handle shares
/// the same `ConnectionMetrics` so observer events report transport-cumulative
/// values, not per-clone values. H1 connections also use this type but never
/// share the `Arc` (each H1 handle owns its own).
pub(crate) struct ConnectionMetrics {
    pub(crate) requests_served: AtomicU32,
    pub(crate) bytes_sent: AtomicU64,
    pub(crate) bytes_received: AtomicU64,
}

impl ConnectionMetrics {
    fn new() -> Self {
        Self {
            requests_served: AtomicU32::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
        }
    }
}

/// An established HTTP connection at a specific protocol version.
pub(crate) enum HttpConnection<B> {
    /// An HTTP/1.1 connection.
    H1(hyper::client::conn::http1::SendRequest<B>),
    /// An HTTP/2 connection.
    H2(hyper::client::conn::http2::SendRequest<B>),
    /// An HTTP/3 connection.
    #[cfg(all(feature = "http3", feature = "rustls"))]
    H3(crate::h3_transport::H3Connection),
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
    /// Per-transport cumulative metrics. Shared across H2/H3 multiplex clones
    /// so observer events report transport totals, not per-handle values.
    pub(crate) metrics: Arc<ConnectionMetrics>,
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
            metrics: Arc::new(ConnectionMetrics::new()),
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
            metrics: Arc::new(ConnectionMetrics::new()),
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
    pub(crate) fn new_h3(sender: crate::h3_transport::H3Connection) -> Self {
        Self {
            conn: HttpConnection::H3(sender),
            remote_addr: None,
            tls_info: None,
            tls_handshake_duration: None,
            sans: Arc::from([]),
            created_at: Instant::now(),
            metrics: Arc::new(ConnectionMetrics::new()),
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
            HttpConnection::H3(s) => s.is_ready(),
        }
    }

    /// Returns true if this is an HTTP/1.1 connection.
    pub(crate) fn is_h1(&self) -> bool {
        matches!(&self.conn, HttpConnection::H1(_))
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) fn is_h3(&self) -> bool {
        matches!(&self.conn, HttpConnection::H3(_))
    }

    /// Whether the transport can return an untouched request when dispatch is
    /// rejected before serialization.
    pub(crate) fn supports_unsent_request_recovery(&self) -> bool {
        match &self.conn {
            HttpConnection::H1(_) | HttpConnection::H2(_) => true,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => false,
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

    #[cfg(all(test, feature = "tokio"))]
    pub(crate) fn active_multiplex_streams(&self) -> Option<usize> {
        self.active_streams
            .as_ref()
            .map(|active| active.load(Ordering::Acquire))
    }

    /// Record a completed request with the given body size.
    pub(crate) fn record_request(&self, body_size: u64) {
        self.metrics
            .bytes_sent
            .fetch_add(body_size, Ordering::Relaxed);
        self.metrics.requests_served.fetch_add(1, Ordering::Relaxed);
    }

    /// Record bytes received from the response body.
    pub(crate) fn record_bytes_received(&self, len: u64) {
        self.metrics
            .bytes_received
            .fetch_add(len, Ordering::Relaxed);
    }

    /// Return cumulative requests served on this transport.
    pub(crate) fn requests_served(&self) -> u32 {
        self.metrics.requests_served.load(Ordering::Relaxed)
    }

    /// Return cumulative bytes sent on this transport.
    pub(crate) fn bytes_sent(&self) -> u64 {
        self.metrics.bytes_sent.load(Ordering::Relaxed)
    }

    /// Return cumulative bytes received on this transport.
    pub(crate) fn bytes_received(&self) -> u64 {
        self.metrics.bytes_received.load(Ordering::Relaxed)
    }
}

impl<B> Drop for PooledConnection<B> {
    fn drop(&mut self) {
        if let Some(ref key) = self.key
            && let Some(pool_inner) = self.pool.upgrade()
            && let Ok(mut inner) = pool_inner.lock()
        {
            super::decrement_active(&mut inner, key);
        }
    }
}

impl<B: 'static> PooledConnection<B> {
    /// Clone the underlying send handle for H2/H3 multiplexing.
    ///
    /// Returns `None` for H1 connections (no multiplexing).
    #[cfg(all(test, feature = "tokio"))]
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
            metrics: Arc::clone(&self.metrics),
            is_multiplex_clone: true,
            active_streams: self.active_streams.clone(),
            _active_stream_permit: Some(active_stream_permit),
            upgrade_handle_local: None,
            pool: self.pool.clone(),
            key: self.key.clone(),
        })
    }
}
