//! Real-time request lifecycle observer for load testing and tracing.
//!
//! The [`RequestObserver`] trait fires at each connection phase transition
//! with monotonic timestamps and diagnostic data. Use it to implement
//! detailed per-phase tracing, load testing metrics, or custom instrumentation.

use std::net::SocketAddr;
use std::time::Duration;

use http::{Method, StatusCode, Uri};

#[cfg(not(feature = "precise-timing"))]
pub use coarsetime::Instant;
#[cfg(feature = "precise-timing")]
pub use std::time::Instant;

#[cfg(feature = "serde")]
mod serde_status_code {
    use http::StatusCode;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(status: &StatusCode, s: S) -> Result<S::Ok, S::Error> {
        status.as_u16().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<StatusCode, D::Error> {
        let code = u16::deserialize(d)?;
        StatusCode::from_u16(code).map_err(serde::de::Error::custom)
    }
}

/// How the connection was obtained for this request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PoolOutcome {
    /// Exact pool key match — connection reused.
    Hit,
    /// RFC 7540 §9.1.1 connection coalescing (SAN match on TLS cert).
    Coalesced,
    /// No pooled connection available; opening a fresh connection.
    Miss,
    /// Pool hit returned a stale connection; retrying on fresh.
    StaleRetry,
}

/// The HTTP protocol negotiated on the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NegotiatedProtocol {
    /// HTTP/1.0 or HTTP/1.1.
    Http1,
    /// HTTP/2 (h2 or h2c).
    Http2,
    /// HTTP/3 (QUIC).
    Http3,
}

/// Direction of data transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TransferDirection {
    /// Client → Server (request body, WebSocket send).
    Upload,
    /// Server → Client (response body, WebSocket receive).
    Download,
}

/// Events fired during HTTP request execution, carrying diagnostic data.
///
/// Each variant represents a phase transition in the request lifecycle.
/// Phases that are skipped (e.g., DNS for pool hits, TLS for plain HTTP)
/// simply don't fire.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RequestPhase {
    /// Request execution has started (after rate limiting, if any).
    Started,

    /// Pool checkout phase completed.
    PoolCheckoutComplete {
        /// Whether the pool had a usable connection.
        outcome: PoolOutcome,
        /// Time spent blocked waiting for / checking the pool.
        blocked_duration: Duration,
    },

    /// DNS resolution completed.
    DnsResolved {
        /// All resolved addresses (may be multiple for Happy Eyeballs).
        addrs: Vec<SocketAddr>,
        /// Time spent in DNS resolution.
        duration: Duration,
    },

    /// TCP connection established.
    TcpConnected {
        /// Remote address selected (after Happy Eyeballs, if applicable).
        remote_addr: SocketAddr,
        /// Time spent in TCP connect.
        duration: Duration,
        /// Protocol negotiated on this connection.
        protocol: NegotiatedProtocol,
    },

    /// TLS handshake completed (only fired for HTTPS connections).
    TlsHandshakeComplete {
        /// Time spent in TLS negotiation.
        duration: Duration,
        /// ALPN protocol negotiated (e.g. "h2", "http/1.1").
        alpn_protocol: Option<String>,
        /// DER-encoded peer certificate, if available.
        peer_certificate_der: Option<Vec<u8>>,
    },

    /// Request headers and body have been fully sent.
    RequestSent {
        /// Time spent sending.
        duration: Duration,
    },

    /// First response byte received (TTFB).
    ResponseStarted {
        /// Time waiting for the server to respond after send completed.
        waiting_duration: Duration,
    },

    /// Response headers fully received.
    ResponseComplete {
        /// HTTP status code.
        #[cfg_attr(feature = "serde", serde(with = "serde_status_code"))]
        status: StatusCode,
        /// Protocol used for the response.
        protocol: NegotiatedProtocol,
        /// Total request duration (start to response headers complete).
        total_duration: Duration,
    },

    /// Request failed with an error.
    Failed {
        /// Error description.
        error: String,
        /// Whether this will trigger a stale-connection retry.
        will_retry: bool,
        /// Duration from start to failure.
        elapsed: Duration,
    },

    /// Bytes transferred (fires per chunk, bidirectional for WebSocket).
    BytesTransferred {
        /// Direction of this transfer.
        direction: TransferDirection,
        /// Bytes in this chunk.
        chunk_bytes: u64,
        /// Cumulative bytes in this direction for this request/connection.
        cumulative_bytes: u64,
        /// Time since transfer started.
        elapsed: Duration,
    },

    /// Transfer complete in one direction.
    TransferComplete {
        /// Direction of the completed transfer.
        direction: TransferDirection,
        /// Total bytes transferred.
        total_bytes: u64,
        /// Total transfer duration.
        transfer_duration: Duration,
        /// Observed throughput in bytes/sec.
        throughput_bytes_per_sec: f32,
    },

    /// Transfer aborted due to an error before completion.
    TransferAborted {
        /// Direction of the aborted transfer.
        direction: TransferDirection,
        /// Bytes successfully transferred before the error.
        bytes_transferred: u64,
        /// Time elapsed since transfer started.
        elapsed: Duration,
        /// Error that caused the abort.
        error: String,
    },
}

/// Event fired at each phase transition during request execution.
#[derive(Debug, Clone)]
pub struct RequestEvent {
    /// HTTP method of the request.
    pub method: Method,
    /// Target URI of the request.
    pub uri: Uri,
    /// The phase that just completed.
    pub phase: RequestPhase,
    /// Monotonic timestamp when this phase completed.
    pub at: Instant,
}

/// Observer for real-time HTTP request lifecycle events.
///
/// Implementors receive callbacks at each connection/request phase with
/// monotonic timestamps and diagnostic data. The observer is shared across
/// all concurrent requests on an [`crate::HttpEngine`], so `on_event` takes `&self`.
///
/// # Thread Safety
///
/// `on_event` may be called concurrently from multiple async tasks.
/// Implementations must be internally synchronized (e.g., using atomics
/// or channels).
///
/// # Performance
///
/// `on_event` must be non-blocking. Heavy processing should be deferred
/// (e.g., send events to a channel for async processing).
pub trait RequestObserver: Send + Sync + 'static {
    /// Called at each phase transition during request execution.
    fn on_event(&self, event: &RequestEvent);

    /// Called for connection-level lifecycle events (not tied to a specific request).
    fn on_connection_event(&self, event: &ConnectionEvent);
}

/// Connection-level event (not tied to a specific request).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ConnectionPhase {
    /// Connection-level metrics (fires at pool checkin or connection close).
    Metrics {
        /// Remote address of the connection.
        remote_addr: SocketAddr,
        /// Protocol version.
        protocol: NegotiatedProtocol,
        /// Approximate bytes sent (from body size hints). Exact counts
        /// are available via per-request `TransferComplete` events.
        bytes_sent: u64,
        /// Approximate bytes received (from Content-Length header). Exact
        /// counts are available via per-request `TransferComplete` events.
        bytes_received: u64,
        /// Connection lifetime.
        connection_age: Duration,
        /// Number of requests served by this connection.
        requests_served: u32,
        /// Whether the connection was closed (vs returned to pool).
        closed: bool,
    },
}

/// Event fired for connection-level lifecycle transitions.
#[derive(Debug, Clone)]
pub struct ConnectionEvent {
    /// The phase that completed.
    pub phase: ConnectionPhase,
    /// Monotonic timestamp.
    pub at: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_outcome_debug() {
        assert_eq!(format!("{:?}", PoolOutcome::Hit), "Hit");
        assert_eq!(format!("{:?}", PoolOutcome::StaleRetry), "StaleRetry");
    }

    #[test]
    fn negotiated_protocol_debug() {
        assert_eq!(format!("{:?}", NegotiatedProtocol::Http1), "Http1");
        assert_eq!(format!("{:?}", NegotiatedProtocol::Http2), "Http2");
        assert_eq!(format!("{:?}", NegotiatedProtocol::Http3), "Http3");
    }

    #[test]
    fn transfer_direction_debug() {
        assert_eq!(format!("{:?}", TransferDirection::Upload), "Upload");
        assert_eq!(format!("{:?}", TransferDirection::Download), "Download");
    }

    #[test]
    fn request_phase_variants_are_constructible() {
        let _ = RequestPhase::Started;
        let _ = RequestPhase::PoolCheckoutComplete {
            outcome: PoolOutcome::Hit,
            blocked_duration: Duration::from_millis(5),
        };
        let _ = RequestPhase::DnsResolved {
            addrs: vec!["127.0.0.1:80".parse().unwrap()],
            duration: Duration::from_millis(10),
        };
        let _ = RequestPhase::TcpConnected {
            remote_addr: "127.0.0.1:80".parse().unwrap(),
            duration: Duration::from_millis(20),
            protocol: NegotiatedProtocol::Http1,
        };
        let _ = RequestPhase::TlsHandshakeComplete {
            duration: Duration::from_millis(30),
            alpn_protocol: Some("h2".into()),
            peer_certificate_der: None,
        };
        let _ = RequestPhase::RequestSent {
            duration: Duration::from_millis(1),
        };
        let _ = RequestPhase::ResponseStarted {
            waiting_duration: Duration::from_millis(50),
        };
        let _ = RequestPhase::ResponseComplete {
            status: StatusCode::OK,
            protocol: NegotiatedProtocol::Http2,
            total_duration: Duration::from_millis(100),
        };
        let _ = RequestPhase::Failed {
            error: "timeout".into(),
            will_retry: false,
            elapsed: Duration::from_secs(5),
        };
        let _ = RequestPhase::BytesTransferred {
            direction: TransferDirection::Download,
            chunk_bytes: 1024,
            cumulative_bytes: 4096,
            elapsed: Duration::from_millis(200),
        };
        let _ = RequestPhase::TransferComplete {
            direction: TransferDirection::Upload,
            total_bytes: 8192,
            transfer_duration: Duration::from_millis(500),
            throughput_bytes_per_sec: 16384.0,
        };
        let _ = RequestPhase::TransferAborted {
            direction: TransferDirection::Download,
            bytes_transferred: 2048,
            elapsed: Duration::from_millis(100),
            error: "connection reset".into(),
        };
        let _ = ConnectionPhase::Metrics {
            remote_addr: "10.0.0.1:443".parse().unwrap(),
            protocol: NegotiatedProtocol::Http2,
            bytes_sent: 1024,
            bytes_received: 65536,
            connection_age: Duration::from_secs(30),
            requests_served: 5,
            closed: false,
        };
    }

    #[test]
    fn request_event_is_constructible() {
        let event = RequestEvent {
            method: Method::GET,
            uri: "http://example.com/".parse().unwrap(),
            phase: RequestPhase::Started,
            at: Instant::now(),
        };
        assert_eq!(event.method, Method::GET);
    }

    use std::sync::{Arc, Mutex};

    struct RecordingObserver {
        events: Arc<Mutex<Vec<RequestPhase>>>,
    }

    impl RequestObserver for RecordingObserver {
        fn on_event(&self, event: &RequestEvent) {
            self.events.lock().unwrap().push(event.phase.clone());
        }

        fn on_connection_event(&self, _event: &ConnectionEvent) {}
    }

    #[test]
    fn recording_observer_captures_events() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer = RecordingObserver {
            events: events.clone(),
        };
        observer.on_event(&RequestEvent {
            method: Method::POST,
            uri: "http://localhost/api".parse().unwrap(),
            phase: RequestPhase::Started,
            at: Instant::now(),
        });
        observer.on_event(&RequestEvent {
            method: Method::POST,
            uri: "http://localhost/api".parse().unwrap(),
            phase: RequestPhase::ResponseComplete {
                status: StatusCode::CREATED,
                protocol: NegotiatedProtocol::Http1,
                total_duration: Duration::from_millis(42),
            },
            at: Instant::now(),
        });
        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(matches!(captured[0], RequestPhase::Started));
        assert!(matches!(captured[1], RequestPhase::ResponseComplete { .. }));
    }
}
