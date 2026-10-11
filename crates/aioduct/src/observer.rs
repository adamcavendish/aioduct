//! Backend-neutral request observations.
//!
//! Configure an [`Observer`] on the engine builder. Aioduct calls
//! [`Observer::begin`] once when a logical request actually executes, and keeps
//! the returned [`RequestObservation`] alive internally through the response
//! body. Returned response and stream types do not depend on the observer type.
//! Exporting to tracing, OTLP, or multiple backends is the user's responsibility.
//!
//! The native 0.3 implementation covers start, selected headers, typed attempt
//! facts, low-frequency connection acquisition, independent channel and
//! connection ownership, finish, and original DATA byte summaries. Upload,
//! remaining acquisition phases and protocol/runtime validation remain in
//! migration. The 0.2 request observer API has been replaced, without a
//! compatibility adapter.
//! Native SSE lifecycle and selected protocol statistics are delivered
//! independently of the HTTP request's terminal summary.
//!
//! ```no_run
//! # #[cfg(all(feature = "tokio", not(target_arch = "wasm32")))]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use aioduct::{Interest, NoopConnectionObservation, Observer,
//!               RequestObservation, RequestInfo, RequestFinish, TokioClient};
//!
//! struct Metrics;
//! struct RequestMetrics { request_id: Option<u64> }
//! impl Observer for Metrics {
//!     type Context = u64;
//!     type Observation = RequestMetrics;
//!     type ConnectionObservation = NoopConnectionObservation;
//!
//!     fn begin(&self, _: RequestInfo<'_>, context: Option<u64>)
//!         -> (Interest, RequestMetrics)
//!     {
//!         (Interest::REQUEST_FINISH | Interest::RESPONSE_BODY_BYTES,
//!          RequestMetrics { request_id: context })
//!     }
//!     fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
//!         (Interest::EMPTY, NoopConnectionObservation)
//!     }
//! }
//! impl RequestObservation for RequestMetrics {
//!     fn on_request_finish(&self, event: &RequestFinish<'_>) {
//!         // Record event.reason, event.response, event.attempt_count,
//!         // event.original_data_bytes and self.request_id in your backend.
//!     }
//! }
//!
//! let client = TokioClient::builder().observer(Metrics).build()?;
//! let response: aioduct::Response = client.get("https://example.com/")?
//!     .observation_context(42).send().await?;
//! let bytes = response.bytes().await?; // Finish follows reliable body completion.
//! let response = client.get("https://example.com/")?
//!     .disable_observation().send().await?; // Skips begin entirely.
//! # Ok(())
//! # }
//! ```

use http::{Method, StatusCode, Uri};
use std::net::SocketAddr;

mod channel;
pub use channel::{
    ChannelDetachReason, ChannelHandoffInfo, ChannelHandoffMode, ChannelIoEvent,
    ChannelIoOperation, ConnectionAssociation, ResponseChannelHandoffEvent,
};

#[cfg(not(target_arch = "wasm32"))]
mod original_data;

#[cfg(not(feature = "precise-timing"))]
pub use coarsetime::Instant;
#[cfg(feature = "precise-timing")]
pub use std::time::Instant;

// ── 0.3 request-level observer contract ─────────────────────────────────────

/// The observation categories a request may explicitly subscribe to.
///
/// This is intentionally represented by one `u32`: selecting a summary does
/// not implicitly enable a high-frequency callback, and an empty interest must
/// not create request state in the execution path.
///
/// During the 0.3 migration, native delivery exists for request start, selected
/// response headers, attempt facts, the initial connection-acquisition phases
/// and acquired-resource facts, request finish, original DATA summaries, and
/// SSE lifecycle/statistics. Other categories remain reserved for their typed
/// callbacks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interest(u32);

impl Interest {
    /// No request-level facts are collected.
    pub const EMPTY: Self = Self(0);
    /// The logical request start callback.
    pub const REQUEST_START: Self = Self(1 << 0);
    /// The selected response headers callback.
    pub const RESPONSE_HEADERS: Self = Self(1 << 1);
    /// The logical request finish callback.
    pub const REQUEST_FINISH: Self = Self(1 << 2);
    /// Attempt diagnostics.
    pub const ATTEMPT: Self = Self(1 << 3);
    /// Request/attempt connection acquisition diagnostics.
    pub const CONNECTION_ACQUISITION: Self = Self(1 << 4);
    /// Request upload diagnostics.
    pub const REQUEST_BODY: Self = Self(1 << 5);
    /// Explicit response-body lifecycle/progress diagnostics.
    pub const RESPONSE_BODY: Self = Self(1 << 6);
    /// Response-body original DATA byte summary.
    pub const RESPONSE_BODY_BYTES: Self = Self(1 << 7);
    /// SSE establishment, original errors, reliable end, and abandonment.
    pub const SSE_LIFECYCLE: Self = Self(1 << 8);
    /// Response-to-channel handoff diagnostics.
    pub const RESPONSE_CHANNEL_HANDOFF: Self = Self(1 << 9);
    /// Post-handoff channel lifecycle and byte summaries.
    pub const CHANNEL_LIFECYCLE: Self = Self(1 << 10);
    /// SSE protocol statistics, delivered on existing lifecycle events.
    /// Internally enables `SSE_LIFECYCLE`, not per-message callbacks.
    pub const SSE_STATISTICS: Self = Self(1 << 11);
    /// Selected cumulative channel delivery/acceptance at lifecycle events.
    /// Internally enables `CHANNEL_LIFECYCLE`, never per-operation callbacks.
    pub const CHANNEL_BYTES: Self = Self(1 << 12);
    /// Shared-connection lifecycle facts.
    pub const CONNECTION_LIFECYCLE: Self = Self(1 << 13);
    /// Connection-establishment diagnostics.
    pub const CONNECTION_ESTABLISHMENT: Self = Self(1 << 14);

    /// Returns the raw interest bits.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns whether all bits in `other` are selected.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns whether any bit in `other` is selected.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Returns the union of two interest sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns whether no category is selected.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn with_dependencies(self) -> Self {
        let mut interest = self;
        if interest.contains(Self::RESPONSE_BODY_BYTES) {
            interest = interest.union(Self::REQUEST_FINISH);
        }
        if interest.contains(Self::SSE_STATISTICS) {
            interest = interest.union(Self::SSE_LIFECYCLE);
        }
        if interest.contains(Self::CHANNEL_BYTES) {
            interest = interest.union(Self::CHANNEL_LIFECYCLE);
        }
        interest
    }
}

impl std::ops::BitOr for Interest {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for Interest {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

/// The request facts available when an observation begins.
#[derive(Debug, Clone, Copy)]
pub struct RequestInfo<'a> {
    /// The method of the logical request.
    pub method: &'a Method,
    /// The initial URI after base-URL resolution.
    pub initial_uri: &'a Uri,
}

/// The logical request start milestone, collected only when subscribed.
#[derive(Debug, Clone, Copy)]
pub struct RequestStart<'a> {
    /// The method of the logical request.
    pub method: &'a Method,
    /// The initial URI after base-URL resolution.
    pub initial_uri: &'a Uri,
    /// The time at which request execution began.
    pub at: Instant,
}

/// The execution branch that supplied the selected response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResponseSource {
    /// The response was selected from the transport execution path.
    Transport,
    /// The response was selected directly from aioduct's HTTP cache.
    CacheHit,
    /// A 304 revalidation selected the cached response.
    CacheRevalidated,
    /// A stale-if-error branch selected the cached response.
    CacheStaleIfError,
}

/// The low-cost identity of the selected final response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseInfo {
    /// The HTTP status code.
    pub status: StatusCode,
    /// The HTTP version used by the selected response, when the execution
    /// backend can observe it. Host-managed fetch/WASI paths may leave it
    /// unknown.
    pub version: Option<http::Version>,
    /// The execution branch that selected the response.
    pub source: ResponseSource,
}

/// The response-headers milestone for a logical request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseHeaders {
    /// The selected response identity.
    pub response: ResponseInfo,
    /// The time at which the milestone occurred.
    pub at: Instant,
}

/// The terminal fact for a logical request.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum RequestFinishReason<'a> {
    /// The selected body and library processing ended reliably.
    Ended,
    /// A real aioduct error ended the request.
    Failed {
        /// The original aioduct error; it is borrowed and not cloned.
        error: &'a crate::error::Error,
    },
    /// A returned but not reliably completed body was released.
    ConsumerAbandoned,
    /// Execution ended before a response was handed to the caller.
    ExecutionAbandoned,
    /// An accepted upgrade or tunnel was successfully handed off.
    HandedOff,
}

/// The low-frequency terminal summary for a logical request.
#[derive(Debug, Clone, Copy)]
pub struct RequestFinish<'a> {
    /// The typed terminal reason.
    pub reason: RequestFinishReason<'a>,
    /// The selected final response, if one exists.
    pub response: Option<ResponseInfo>,
    /// The number of HTTP attempts that actually started.
    pub attempt_count: u64,
    /// Original DATA bytes, when `RESPONSE_BODY_BYTES` was selected and the
    /// response body was applicable.
    pub original_data_bytes: Option<u64>,
    /// The time at which the terminal fact occurred.
    pub at: Instant,
}

/// A phase of a physical connection's establishment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionEstablishmentPhase {
    /// Name resolution for the connection's endpoint.
    Dns,
    /// A TCP connection attempt.
    TcpConnect,
    /// A QUIC connection attempt.
    QuicConnect,
    /// TLS handshake and protocol negotiation.
    TlsHandshake,
    /// An internal proxy CONNECT negotiation.
    ProxyConnect,
}

/// The transport used by one connection-local connect attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportKind {
    /// A TCP connection, including a TCP connection later wrapped in TLS.
    Tcp,
    /// A QUIC connection used by HTTP/3.
    Quic,
}

/// The stable facts identifying one candidate connection attempt.
///
/// `sequence` is local to one physical connection establishment and is not an
/// HTTP attempt number or a process-wide identifier. The address is a
/// diagnostic attribute, not a connection identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectAttempt {
    /// The candidate's ordinal within one connection establishment.
    pub sequence: u32,
    /// The underlying transport used for this candidate.
    pub kind: TransportKind,
    /// The resolved remote address targeted by this candidate.
    pub remote_addr: SocketAddr,
}

/// A low-frequency fact belonging to one physical connection observation.
///
/// The connection owner is independent from every request and stream using
/// the connection.  All callback data is borrowed for the duration of the
/// synchronous callback; aioduct does not retain user-owned errors.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ConnectionObservationEvent<'a> {
    /// A resolved address candidate began its underlying connection attempt.
    ConnectAttemptStarted {
        /// The candidate facts.
        attempt: ConnectAttempt,
        /// The monotonic time at which the candidate began.
        at: Instant,
    },
    /// A candidate's underlying TCP/QUIC connection succeeded.
    ///
    /// This does not by itself mean that TLS or the HTTP protocol handshake
    /// succeeded; `Established` remains the usable-connection milestone.
    ConnectAttemptSucceeded {
        /// The candidate facts.
        attempt: ConnectAttempt,
        /// The monotonic time at which the underlying connect succeeded.
        at: Instant,
    },
    /// A candidate's underlying connection attempt failed.
    ConnectAttemptFailed {
        /// The candidate facts.
        attempt: ConnectAttempt,
        /// The original aioduct error for this candidate.
        error: &'a crate::error::Error,
        /// The monotonic time at which the candidate failed.
        at: Instant,
    },
    /// A candidate was released before its underlying connect completed.
    ///
    /// In a Happy Eyeballs race this normally means another candidate won;
    /// it does not mean that the complete connection establishment was
    /// cancelled.
    ConnectAttemptCancelled {
        /// The candidate facts.
        attempt: ConnectAttempt,
        /// The monotonic time at which the candidate was cancelled.
        at: Instant,
    },
    /// A visible connection-establishment phase began.
    PhaseStarted {
        /// The establishment phase that began.
        phase: ConnectionEstablishmentPhase,
        /// The monotonic time at which the phase began.
        at: Instant,
    },
    /// A visible connection-establishment phase completed.
    PhaseCompleted {
        /// The establishment phase that completed.
        phase: ConnectionEstablishmentPhase,
        /// The monotonic time at which the phase completed.
        at: Instant,
    },
    /// The connection could not become usable.
    EstablishmentFailed {
        /// The original aioduct error.
        error: &'a crate::error::Error,
        /// The monotonic time at which establishment failed.
        at: Instant,
    },
    /// Establishment was cancelled before a usable connection existed.
    EstablishmentCancelled {
        /// The monotonic time at which establishment was cancelled.
        at: Instant,
    },
    /// The connection became usable and received its stable association.
    Established {
        /// The stable identity assigned to the physical connection.
        connection: ConnectionAssociation,
        /// The negotiated HTTP version.
        version: http::Version,
        /// The monotonic time at which the connection became usable.
        at: Instant,
    },
    /// The connection's eligibility for new requests changed.
    PoolEligibilityChanged {
        /// Whether the pool may offer this connection to new requests.
        reusable: bool,
        /// Monotonically increasing connection-state revision.
        revision: u64,
        /// The monotonic time at which eligibility changed.
        at: Instant,
    },
    /// The managed connection ended with an optional original error.
    Closed {
        /// The original aioduct error, when closure was error-driven.
        error: Option<&'a crate::error::Error>,
        /// The monotonic time at which the connection closed.
        at: Instant,
    },
    /// The connection left aioduct's managed I/O path without proof of close.
    Detached {
        /// The monotonic time at which the connection detached.
        at: Instant,
    },
}

/// Internal projection used by the transport race to report candidate facts
/// without exposing connection-owner state to the runtime connector.
pub(crate) trait ConnectAttemptObserver {
    fn connect_attempt_started(&self, attempt: ConnectAttempt);
    fn connect_attempt_succeeded(&self, attempt: ConnectAttempt);
    fn connect_attempt_failed(&self, attempt: ConnectAttempt, error: &crate::error::Error);
    fn connect_attempt_cancelled(&self, attempt: ConnectAttempt);
}

/// A connection-level observation owned by the pool/connection layer.
pub trait ConnectionObservation: 'static {
    /// Reports one low-frequency physical connection fact.
    fn on_connection(&self, _event: &ConnectionObservationEvent<'_>) {}
}

#[cfg(not(target_arch = "wasm32"))]
mod connection_owner;
#[cfg(any(target_arch = "wasm32", feature = "wasm", feature = "wasi-p2"))]
mod host;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod transport;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use connection_owner::{
    ConnectionObservationOwner, EligibilityChange, LocalConnectionObservationOwner,
};
#[cfg(any(target_arch = "wasm32", feature = "wasm", feature = "wasi-p2"))]
pub(crate) use host::HostRequestObservation;

/// A request-level observation created by [`Observer::begin`].
///
/// Callback methods are synchronous and receive `&self`. Exporters, queues,
/// locking, and fan-out remain the observer implementation's responsibility.
/// Aioduct releases its internal locks before calling these methods. Panics
/// propagate normally; callbacks should be short and non-blocking.
pub trait RequestObservation: 'static {
    /// Reports the logical request start when `Interest::REQUEST_START` is set.
    fn on_request_start(&self, _event: &RequestStart<'_>) {}

    /// Reports the selected response before aioduct's body processing begins.
    fn on_response_headers(&self, _event: &ResponseHeaders) {}

    /// Reports the logical request's single terminal summary.
    fn on_request_finish(&self, _event: &RequestFinish<'_>) {}

    /// Reports attempt execution and intermediate-response disposition.
    fn on_attempt(&self, _event: &AttemptEvent<'_>) {}

    /// Reports request/attempt connection acquisition facts.
    fn on_connection_acquisition(&self, _event: &ConnectionAcquisitionEvent<'_>) {}

    /// Reports low-frequency request upload facts.
    fn on_request_body(&self, _event: &RequestBodyEvent<'_>) {}

    /// Reports low-frequency response-body terminal facts.
    fn on_response_body(&self, _event: &ResponseBodyEvent<'_>) {}

    /// Reports independent SSE lifecycle facts, not successful reads/messages.
    /// Errors and statistics are borrowed only for this synchronous callback.
    fn on_response_sse(&self, _event: ResponseSseEvent<'_>) {}

    /// Reports actual acquisition of an accepted response's channel.
    fn on_response_channel_handoff(&self, _event: &ResponseChannelHandoffEvent<'_>) {}

    /// Reports independent channel errors, EOF, shutdown and detachment.
    fn on_channel_io(&self, _event: &ChannelIoEvent<'_>) {}
}

/// Selected SSE protocol statistics, not application or network-wire metrics.
///
/// Counts saturate at `u64::MAX`. Buffer lengths describe pending parser input,
/// not capacity, delivered message sizes, or total memory usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SseStatisticsSnapshot {
    /// Successfully returned Message events, including empty/custom messages.
    pub messages_delivered: u64,
    /// Comment lines actually recognized by the parser; discarded input is excluded.
    pub comment_lines_parsed: u64,
    /// Retry fields successfully parsed as representable intervals, not HTTP retries.
    pub retry_fields_accepted: u64,
    /// Input bytes still held for parsing, including unclosed trailing input.
    pub buffered_input_bytes: usize,
    /// Maximum pending input length within this SSE lifecycle.
    pub peak_buffered_input_bytes: usize,
}

/// A low-frequency SSE fact with its own lifetime, independent of HTTP finish.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ResponseSseEvent<'a> {
    /// The associated SSE stream was established, before any read is needed.
    Started,
    /// An original error was returned; this does not terminate the SSE stream.
    Error {
        /// The same aioduct error that is returned to the caller, without cloning.
        error: &'a crate::error::Error,
        /// Selected statistics, or None when statistics were not collected.
        statistics: Option<&'a SseStatisticsSnapshot>,
    },
    /// The stream reliably confirmed there is no next event.
    Ended {
        /// Final selected statistics, including unprocessed trailing input.
        statistics: Option<&'a SseStatisticsSnapshot>,
    },
    /// The stream was released before reliable SSE end was confirmed.
    Abandoned {
        /// Selected statistics at release; remaining input need not be malformed.
        statistics: Option<&'a SseStatisticsSnapshot>,
    },
}

/// The reason an intermediate response was not delivered to the caller.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ResponseDisposition {
    Retry,
    Redirect,
    AuthenticationReplay,
    CacheRevalidation,
    StaleIfError,
}

/// Why an attempt became eligible for another execution.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum RetryReason<'a> {
    ResponseStatus(StatusCode),
    Error { error: &'a crate::error::Error },
    StaleConnection,
    Policy,
}

/// The actual state of an inter-attempt wait, independently of scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackoffState {
    /// The configured wait has begun.
    Started,
    /// The wait completed; this does not mean another attempt has begun.
    Completed,
    /// Execution was released while waiting.
    Cancelled,
}

/// Admission by the configured request rate limiter, independently of the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateLimitState {
    /// This attempt began trying to obtain execution eligibility.
    Started,
    /// The limiter admitted execution, possibly without a wait.
    Admitted,
    /// Execution was released before admission.
    Cancelled,
}

/// The result of internal disposal of an intermediate response body.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum CleanupState<'a> {
    /// The body reached a reliable end.
    Drained,
    /// Aioduct stopped reading, including cancellation of a drain.
    Stopped,
    /// Internal reading failed, independently of the disposition decision.
    Failed {
        /// The original error, borrowed for the callback only.
        error: &'a crate::error::Error,
    },
}

/// The low-frequency facts belonging to one logical HTTP attempt.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum AttemptEvent<'a> {
    Started {
        attempt: u64,
        method: &'a Method,
        uri: &'a Uri,
        at: Instant,
    },
    /// One admission operation, not each rejected check or internal sleep.
    RateLimit {
        attempt: u64,
        state: RateLimitState,
        at: Instant,
    },
    ResponseDisposition {
        attempt: u64,
        disposition: ResponseDisposition,
        response: ResponseInfo,
        at: Instant,
    },
    Failed {
        attempt: u64,
        error: &'a crate::error::Error,
        at: Instant,
    },
    RetryScheduled {
        attempt: u64,
        next_attempt: u64,
        reason: RetryReason<'a>,
        at: Instant,
    },
    Backoff {
        attempt: u64,
        delay: std::time::Duration,
        state: BackoffState,
        at: Instant,
    },
    CleanupStarted {
        attempt: u64,
        at: Instant,
    },
    Cleanup {
        attempt: u64,
        state: CleanupState<'a>,
        /// Successfully collected body bytes, when known; not wire bytes.
        bytes: Option<u64>,
        at: Instant,
    },
}

/// The phase of request-side connection acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ConnectionAcquisitionPhase {
    PoolLookup,
    PoolWait,
    Dns,
    TcpConnect,
    QuicConnect,
    TlsHandshake,
    ProxyConnect,
    StreamAcquire,
}

/// Why a connection-acquisition phase did not occur for this request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum PhaseSkipReason {
    NotApplicable,
    BypassedByConfiguration,
    Unsupported,
}

/// The resource that a request actually obtained from connection management.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ConnectionAcquisitionResource {
    /// An HTTP/1 connection that is used exclusively by this request.
    Connection,
    /// A stream obtained from a multiplexed HTTP/2 or HTTP/3 connection.
    Stream,
}

/// The result of looking for a reusable connection before a request attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum PoolLookupResult {
    Hit,
    Coalesced,
    Miss,
}

/// The request-level connection acquisition facts.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ConnectionAcquisitionEvent<'a> {
    /// A pool lookup completed with its actual candidate result.
    PoolLookup {
        /// The owning HTTP execution attempt, not a physical address candidate.
        attempt: u64,
        result: PoolLookupResult,
        at: Instant,
    },
    PhaseStarted {
        /// The owning HTTP execution attempt, not a physical address candidate.
        attempt: u64,
        phase: ConnectionAcquisitionPhase,
        at: Instant,
    },
    PhaseCompleted {
        /// The owning HTTP execution attempt, not a physical address candidate.
        attempt: u64,
        phase: ConnectionAcquisitionPhase,
        at: Instant,
    },
    Failed {
        /// The owning HTTP execution attempt, not a physical address candidate.
        attempt: u64,
        phase: ConnectionAcquisitionPhase,
        error: &'a crate::error::Error,
        at: Instant,
    },
    Cancelled {
        /// The owning HTTP execution attempt, not a physical address candidate.
        attempt: u64,
        phase: ConnectionAcquisitionPhase,
        at: Instant,
    },
    PhaseSkipped {
        /// The owning HTTP execution attempt, not a physical address candidate.
        attempt: u64,
        phase: ConnectionAcquisitionPhase,
        reason: PhaseSkipReason,
    },
    /// The request obtained a usable connection or multiplexed stream.
    ///
    /// The association identifies the shared physical connection. It is
    /// allocated lazily only when this acquisition event is subscribed to;
    /// it is not a handle and does not extend the connection lifetime.
    ConnectionAcquired {
        /// The owning HTTP execution attempt, not a physical address candidate.
        attempt: u64,
        resource: ConnectionAcquisitionResource,
        connection: super::ConnectionAssociation,
        version: http::Version,
        at: Instant,
    },
}

/// Low-frequency request upload facts.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum RequestBodyEvent<'a> {
    Ended {
        /// The logical-request attempt whose source reached the end.
        attempt: u64,
        bytes: Option<u64>,
        at: Instant,
    },
    Failed {
        /// The logical-request attempt whose source returned the error.
        attempt: u64,
        error: &'a crate::error::Error,
        at: Instant,
    },
}

/// Low-frequency response-body facts. This excludes per-frame progress.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum ResponseBodyEvent<'a> {
    Ended {
        at: Instant,
    },
    Failed {
        error: &'a crate::error::Error,
        at: Instant,
    },
    Abandoned {
        at: Instant,
    },
    HandedOff {
        at: Instant,
    },
}

/// A backend-neutral provider for request-level observations.
///
/// The client/engine selects this type with a default [`NoopObserver`]. The
/// associated context and observation types stay attached to the observer;
/// they do not become additional public response or stream type parameters.
pub trait Observer: 'static {
    /// Per-request context supplied by the caller before execution.
    type Context: 'static;
    /// Request-level observation state owned by aioduct after `begin`.
    type Observation: RequestObservation;
    /// Connection-level observation state owned by aioduct's pool/connection
    /// layer. It is independent of `type Observation` and of request disable.
    type ConnectionObservation: ConnectionObservation;

    /// Create the request observation and choose the facts to collect.
    ///
    /// Called before cache lookup, once across retries and redirects. An absent
    /// context still permits observation; `disable_observation()` skips this
    /// method. An empty interest drops the returned observation immediately
    /// without creating a continuation, clock, guard, or background task.
    fn begin(
        &self,
        request: RequestInfo<'_>,
        context: Option<Self::Context>,
    ) -> (Interest, Self::Observation);

    /// Create a physical-connection observation when a new managed connection
    /// starts establishment. The returned interest uses
    /// `CONNECTION_LIFECYCLE` and/or `CONNECTION_ESTABLISHMENT`.
    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation);
}

/// The default zero-sized observer.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopObserver;

/// The default zero-sized request context.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopContext;

/// The default zero-sized request observation.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopRequestObservation;

/// The default zero-sized connection observation.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopConnectionObservation;

impl RequestObservation for NoopRequestObservation {}
impl ConnectionObservation for NoopConnectionObservation {}

impl Observer for NoopObserver {
    type Context = NoopContext;
    type Observation = NoopRequestObservation;
    type ConnectionObservation = NoopConnectionObservation;

    #[inline]
    fn begin(
        &self,
        _request: RequestInfo<'_>,
        _context: Option<Self::Context>,
    ) -> (Interest, Self::Observation) {
        (Interest::EMPTY, NoopRequestObservation)
    }

    #[inline]
    fn begin_connection(&self) -> (Interest, NoopConnectionObservation) {
        (Interest::EMPTY, NoopConnectionObservation)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod continuation;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use continuation::{
    AcquisitionPhases, AttemptStartObserver, LocalAttemptObserver, LocalRequestObservationHandle,
    NoopAttemptStartObserver, RequestAttemptCounter, SendAttemptObserver,
    SendRequestObservationHandle,
};
