use http::Uri;
use std::net::SocketAddr;

/// Boxed error type for dynamic dispatch.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Errors that can occur during HTTP operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error from the `http` crate (e.g., invalid headers or status).
    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),

    /// An error from hyper's HTTP transport layer.
    #[error("hyper error: {0}")]
    Hyper(#[from] hyper::Error),

    /// An I/O error (connection refused, broken pipe, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A TLS handshake or protocol error.
    #[error("TLS error: {0}")]
    Tls(#[source] BoxError),

    /// The request timed out.
    #[error("request timeout")]
    Timeout,

    /// The connection attempt timed out.
    #[error("connect timeout")]
    ConnectTimeout,

    /// Reading the response timed out.
    #[error("read timeout")]
    ReadTimeout,

    /// Writing the request body timed out.
    #[error("write timeout")]
    WriteTimeout,

    /// The URL is invalid or cannot be resolved.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// The response had a 4xx or 5xx status code.
    #[error("HTTP status error: {0}")]
    Status(http::StatusCode),

    /// The redirect did not include a valid Location header.
    #[error("redirect error: {0}")]
    Redirect(String),

    /// Too many redirects were followed.
    #[error("too many redirects (max {0})")]
    TooManyRedirects(usize),

    /// HTTPS-only mode rejected a non-HTTPS URL.
    #[error("HTTPS required but URL scheme is {0}")]
    HttpsOnly(String),

    /// An invalid header name or value was encountered.
    #[error("invalid header: {0}")]
    InvalidHeader(String),

    /// The requested operation is not supported by this runtime or transport.
    #[error("unsupported operation: {0}")]
    Unsupported(String),

    /// A connection pool error, such as a configured limit being reached.
    #[error(transparent)]
    Pool(#[from] PoolError),

    /// An HTTP Message Signatures error.
    #[error(transparent)]
    MessageSignature(#[from] crate::message_signatures::MessageSignatureError),

    /// A catch-all for other errors.
    #[error("{0}")]
    Other(#[source] BoxError),

    /// A transport error associated with the resolved remote address.
    #[error("{source} (remote address: {remote_addr})")]
    RemoteAddr {
        /// The remote address used by the failing transport operation.
        remote_addr: SocketAddr,
        /// The underlying transport error.
        #[source]
        source: BoxError,
    },
}

/// Cloneable error recorded by fluent builders whose setter methods return
/// `Self` and therefore cannot fail immediately.
#[derive(Debug, Clone)]
pub(crate) enum BuilderError {
    InvalidHeader(String),
    InvalidUrl(String),
    #[cfg(any(feature = "wasm", feature = "wasi-p2"))]
    Unsupported(String),
}

impl BuilderError {
    pub(crate) fn invalid_header(message: impl Into<String>) -> Self {
        Self::InvalidHeader(message.into())
    }

    pub(crate) fn invalid_url(message: impl Into<String>) -> Self {
        Self::InvalidUrl(message.into())
    }

    pub(crate) fn set_once(slot: &mut Option<Self>, error: Self) {
        if slot.is_none() {
            *slot = Some(error);
        }
    }

    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::InvalidHeader(message) => Error::InvalidHeader(message),
            Self::InvalidUrl(message) => Error::InvalidUrl(message),
            #[cfg(any(feature = "wasm", feature = "wasi-p2"))]
            Self::Unsupported(message) => Error::Unsupported(message),
        }
    }
}

/// Errors originating from the connection pool.
///
/// This type is exposed through [`Error::Pool`]. It is `#[non_exhaustive]` so
/// new pool failure modes (for example acquire timeouts or pool shutdown) can
/// be added without a breaking change. Match with a wildcard arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PoolError {
    /// A configured pool limit prevented a request from proceeding.
    #[error(transparent)]
    Limit(#[from] PoolLimitError),
}

/// A configured pool limit prevented a request from proceeding.
///
/// This is client-side backpressure, not a network failure: it does not
/// classify as a connect failure or a timeout. Callers can detect it with
/// [`Error::is_pool_limit`] and respond by queuing, retrying later, or lowering
/// concurrency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLimitError {
    kind: PoolLimitKind,
    limit: Option<usize>,
}

impl PoolLimitError {
    pub(crate) fn new(kind: PoolLimitKind, limit: Option<usize>) -> Self {
        Self { kind, limit }
    }

    /// The specific limit that was reached.
    pub fn kind(&self) -> PoolLimitKind {
        self.kind
    }

    /// The configured limit value, when known.
    pub fn limit(&self) -> Option<usize> {
        self.limit
    }
}

impl std::fmt::Display for PoolLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let description = match self.kind {
            PoolLimitKind::MaxActivePerHost => "max active connections per host reached",
        };
        match self.limit {
            Some(limit) => write!(f, "pool limit reached: {description} (limit: {limit})"),
            None => write!(f, "pool limit reached: {description}"),
        }
    }
}

impl std::error::Error for PoolLimitError {}

/// The specific pool limit that was reached.
///
/// `#[non_exhaustive]` so additional limit kinds (for example a global
/// connection cap or a pending-acquire cap) can be added without a breaking
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PoolLimitKind {
    /// The `pool_max_active_per_host` cap was reached for the target pool key.
    MaxActivePerHost,
}

impl From<PoolLimitError> for Error {
    fn from(error: PoolLimitError) -> Self {
        Error::Pool(PoolError::Limit(error))
    }
}

/// An error paired with the URL that was being requested.
///
/// Returned by [`RequestBuilderSend::send()`](crate::request::RequestBuilderSend::send)
/// to provide context about which URL caused the failure.
#[derive(Debug)]
pub struct SendError {
    error: Error,
    url: Uri,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(root) = self.error.hidden_root_cause() {
            write!(
                f,
                "{}: {} for url ({})",
                self.error,
                root,
                redact_url(&self.url)
            )
        } else {
            write!(f, "{} for url ({})", self.error, redact_url(&self.url))
        }
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl SendError {
    pub(crate) fn new(error: Error, url: Uri) -> Self {
        Self { error, url }
    }

    /// Returns the URL that was being requested when this error occurred.
    pub fn url(&self) -> &Uri {
        &self.url
    }

    /// Returns a reference to the underlying error.
    pub fn error(&self) -> &Error {
        &self.error
    }

    /// Consumes this error and returns the underlying [`Error`].
    pub fn into_error(self) -> Error {
        self.error
    }

    /// Returns `true` if the underlying error is a timeout.
    pub fn is_timeout(&self) -> bool {
        self.error.is_timeout()
    }

    /// Returns `true` if the underlying error is a connect failure.
    pub fn is_connect(&self) -> bool {
        self.error.is_connect()
    }

    /// Returns `true` if the underlying error is a DNS resolution failure.
    pub fn is_dns(&self) -> bool {
        self.error.is_dns()
    }

    /// Returns `true` if the underlying error indicates a reused connection was closed.
    pub fn is_closed(&self) -> bool {
        self.error.is_closed()
    }

    /// Returns `true` if the underlying error is an HTTP status error.
    pub fn is_status(&self) -> bool {
        self.error.is_status()
    }

    /// Returns `true` if the underlying error is a redirect error.
    pub fn is_redirect(&self) -> bool {
        self.error.is_redirect()
    }

    /// Returns the status code if the underlying error is a status error.
    pub fn status(&self) -> Option<http::StatusCode> {
        self.error.status()
    }

    /// Returns `true` if the underlying error originates from the connection pool.
    pub fn is_pool(&self) -> bool {
        self.error.is_pool()
    }

    /// Returns `true` if the underlying error is a connection pool limit.
    pub fn is_pool_limit(&self) -> bool {
        self.error.is_pool_limit()
    }

    /// Returns the [`PoolError`] if the underlying error is a pool error.
    pub fn pool_error(&self) -> Option<&PoolError> {
        self.error.pool_error()
    }

    /// Returns the [`PoolLimitError`] if the underlying error is a pool limit failure.
    pub fn pool_limit(&self) -> Option<&PoolLimitError> {
        self.error.pool_limit()
    }

    /// Returns the deepest source in the underlying error chain.
    pub fn root_cause(&self) -> &(dyn std::error::Error + 'static) {
        self.error.root_cause()
    }

    /// Returns the remote address associated with the underlying transport error, if known.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.error.remote_addr()
    }
}

impl From<SendError> for Error {
    fn from(e: SendError) -> Self {
        e.error
    }
}

impl Error {
    pub(crate) fn with_remote_addr(self, remote_addr: SocketAddr) -> Self {
        match self {
            Error::RemoteAddr { .. } => self,
            source => Error::RemoteAddr {
                remote_addr,
                source: Box::new(source),
            },
        }
    }

    /// Returns the deepest source in this error's chain, or this error if it has no source.
    pub fn root_cause(&self) -> &(dyn std::error::Error + 'static) {
        let mut source = self as &(dyn std::error::Error + 'static);
        while let Some(next) = source.source() {
            source = next;
        }
        source
    }

    /// Returns `true` if the error is a network-level failure (I/O, TLS, timeout).
    pub fn is_connect(&self) -> bool {
        match self {
            Error::Io(_) | Error::Tls(_) | Error::ConnectTimeout => true,
            Error::RemoteAddr { source, .. } => {
                source
                    .downcast_ref::<Error>()
                    .is_some_and(Error::is_connect)
                    || source
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(is_connect_io_error)
            }
            Error::Hyper(e) => {
                // A hyper error is a "connect" failure when it wraps an I/O
                // error that indicates the connection was refused, reset, or
                // otherwise could not be established.
                let mut source = std::error::Error::source(e);
                while let Some(err) = source {
                    if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
                        return is_connect_io_error(io_err);
                    }
                    source = err.source();
                }
                false
            }
            _ => false,
        }
    }

    /// Returns `true` if the error is a timeout.
    pub fn is_timeout(&self) -> bool {
        match self {
            Error::Timeout | Error::ConnectTimeout | Error::ReadTimeout | Error::WriteTimeout => {
                true
            }
            Error::RemoteAddr { source, .. } => {
                source
                    .downcast_ref::<Error>()
                    .is_some_and(Error::is_timeout)
                    || source
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::TimedOut)
            }
            _ => false,
        }
    }

    /// Returns the remote address associated with this transport error, if known.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        match self {
            Error::RemoteAddr { remote_addr, .. } => Some(*remote_addr),
            _ => None,
        }
    }

    /// Returns `true` if the error is an HTTP status error.
    pub fn is_status(&self) -> bool {
        matches!(self, Error::Status(_))
    }

    /// Returns the status code if this is a [`Error::Status`] variant.
    pub fn status(&self) -> Option<http::StatusCode> {
        match self {
            Error::Status(code) => Some(*code),
            _ => None,
        }
    }

    /// Returns `true` if the error is a redirect error.
    pub fn is_redirect(&self) -> bool {
        matches!(self, Error::Redirect(_) | Error::TooManyRedirects(_))
    }

    /// Returns `true` if the error was caused by a DNS resolution failure.
    pub fn is_dns(&self) -> bool {
        match self {
            Error::RemoteAddr { source, .. } => {
                source.downcast_ref::<Error>().is_some_and(Error::is_dns)
            }
            Error::Io(e) => {
                // OS DNS errors on Linux (glibc): "Name or service not known"
                // OS DNS errors on macOS: "nodename nor servname provided"
                let msg = e.to_string();
                msg.contains("dns")
                    || msg.contains("resolve")
                    || msg.contains("Name or service not known")
                    || msg.contains("nodename nor servname")
                    || msg.contains("no DNS resolver")
            }
            Error::Hyper(e) => {
                // Walk the Hyper source chain for I/O DNS errors
                let mut source = std::error::Error::source(e);
                while let Some(err) = source {
                    if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
                        let msg = io_err.to_string();
                        return msg.contains("dns")
                            || msg.contains("resolve")
                            || msg.contains("Name or service not known")
                            || msg.contains("nodename nor servname");
                    }
                    source = err.source();
                }
                false
            }
            Error::InvalidUrl(msg) => {
                msg.contains("no DNS resolver") || msg.contains("cannot resolve")
            }
            _ => false,
        }
    }

    /// Returns `true` if the error indicates a reused connection was closed by the peer.
    ///
    /// This covers both TCP-level closes (RST, FIN) and HTTP-level closes
    /// (GOAWAY, canceled requests). Useful for distinguishing "stale pool
    /// connection" errors from genuine server-side failures.
    pub fn is_closed(&self) -> bool {
        use std::error::Error as _;
        match self {
            Error::RemoteAddr { source, .. } => {
                source.downcast_ref::<Error>().is_some_and(Error::is_closed)
            }
            Error::Hyper(e) => {
                if e.is_canceled() || e.is_closed() || e.is_incomplete_message() {
                    return true;
                }
                if let Some(io_err) = e.source().and_then(|s| s.downcast_ref::<std::io::Error>()) {
                    return matches!(
                        io_err.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionAborted
                    );
                }
                false
            }
            Error::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
            ),
            _ => false,
        }
    }

    /// Returns `true` if the error is a write timeout during body upload.
    pub fn is_write_timeout(&self) -> bool {
        match self {
            Error::WriteTimeout => true,
            Error::RemoteAddr { source, .. } => source
                .downcast_ref::<Error>()
                .is_some_and(Error::is_write_timeout),
            _ => false,
        }
    }

    /// Returns `true` if the error originates from the connection pool.
    pub fn is_pool(&self) -> bool {
        match self {
            Error::Pool(_) => true,
            Error::RemoteAddr { source, .. } => {
                source.downcast_ref::<Error>().is_some_and(Error::is_pool)
            }
            _ => false,
        }
    }

    /// Returns `true` if the error is a connection pool limit (client-side
    /// backpressure), such as `pool_max_active_per_host` being reached.
    ///
    /// This is not a network failure: it does not satisfy [`is_connect`] or
    /// [`is_timeout`]. Use it to queue, retry later, or reduce concurrency.
    ///
    /// [`is_connect`]: Error::is_connect
    /// [`is_timeout`]: Error::is_timeout
    pub fn is_pool_limit(&self) -> bool {
        match self {
            Error::Pool(PoolError::Limit(_)) => true,
            Error::RemoteAddr { source, .. } => source
                .downcast_ref::<Error>()
                .is_some_and(Error::is_pool_limit),
            _ => false,
        }
    }

    /// Returns the [`PoolError`] if this is a [`Error::Pool`] variant.
    pub fn pool_error(&self) -> Option<&PoolError> {
        match self {
            Error::Pool(e) => Some(e),
            Error::RemoteAddr { source, .. } => {
                source.downcast_ref::<Error>().and_then(Error::pool_error)
            }
            _ => None,
        }
    }

    /// Returns the [`PoolLimitError`] if this error is a pool limit failure.
    pub fn pool_limit(&self) -> Option<&PoolLimitError> {
        match self {
            Error::Pool(PoolError::Limit(e)) => Some(e),
            Error::RemoteAddr { source, .. } => {
                source.downcast_ref::<Error>().and_then(Error::pool_limit)
            }
            _ => None,
        }
    }

    fn hidden_root_cause(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let mut source = std::error::Error::source(self)?;
        let mut nested = false;

        while let Some(next) = source.source() {
            nested = true;
            source = next;
        }

        if nested && !self.to_string().contains(&source.to_string()) {
            Some(source)
        } else {
            None
        }
    }
}

fn is_connect_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::AddrNotAvailable
            | std::io::ErrorKind::AddrInUse
            | std::io::ErrorKind::NotConnected
    )
}

fn redact_url(uri: &Uri) -> String {
    if let Some(authority) = uri.authority() {
        if authority.as_str().contains('@') {
            let host_port = authority.host().to_owned()
                + &authority
                    .port()
                    .map(|p| format!(":{p}"))
                    .unwrap_or_default();
            format!(
                "{}://[redacted]@{}{}",
                uri.scheme_str().unwrap_or("http"),
                host_port,
                uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
            )
        } else {
            uri.to_string()
        }
    } else {
        uri.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer layer")]
    struct OuterLayer {
        #[source]
        source: InnerLayer,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("inner cause")]
    struct InnerLayer;

    #[test]
    fn is_connect_for_io() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert!(err.is_connect());
        assert!(!err.is_status());
        assert!(!err.is_timeout());
        assert!(!err.is_redirect());
    }

    #[test]
    fn is_connect_for_tls() {
        let err = Error::Tls("bad cert".into());
        assert!(err.is_connect());
    }

    #[test]
    fn is_connect_for_connect_timeout() {
        let err = Error::ConnectTimeout;
        assert!(err.is_connect());
        assert!(err.is_timeout());
    }

    #[test]
    fn read_timeout_not_connect() {
        let err = Error::ReadTimeout;
        assert!(!err.is_connect());
        assert!(err.is_timeout());
    }

    #[test]
    fn generic_timeout_not_connect() {
        let err = Error::Timeout;
        assert!(!err.is_connect());
        assert!(err.is_timeout());
    }

    #[test]
    fn is_status_and_status_accessor() {
        let err = Error::Status(http::StatusCode::NOT_FOUND);
        assert!(err.is_status());
        assert_eq!(err.status(), Some(http::StatusCode::NOT_FOUND));
    }

    #[test]
    fn status_returns_none_for_non_status() {
        let err = Error::Timeout;
        assert_eq!(err.status(), None);
    }

    #[test]
    fn is_redirect_for_redirect() {
        let err = Error::Redirect("missing Location".into());
        assert!(err.is_redirect());
    }

    #[test]
    fn is_redirect_for_too_many() {
        let err = Error::TooManyRedirects(10);
        assert!(err.is_redirect());
    }

    #[test]
    fn pool_limit_error_classification_and_details() {
        let limit = PoolLimitError::new(PoolLimitKind::MaxActivePerHost, Some(1));
        let err = Error::from(PoolError::from(limit.clone()));

        assert!(err.is_pool());
        assert!(err.is_pool_limit());
        assert!(!err.is_connect());
        assert!(!err.is_timeout());
        assert_eq!(
            err.pool_error().map(|e| e.to_string()),
            Some(limit.to_string())
        );
        assert_eq!(
            err.pool_limit().map(PoolLimitError::kind),
            Some(PoolLimitKind::MaxActivePerHost)
        );
        assert_eq!(err.pool_limit().and_then(PoolLimitError::limit), Some(1));
        assert!(
            err.to_string()
                .contains("max active connections per host reached")
        );
    }

    #[test]
    fn send_error_pool_limit_delegates_to_inner_error() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(
            Error::from(PoolError::from(PoolLimitError::new(
                PoolLimitKind::MaxActivePerHost,
                Some(2),
            ))),
            uri,
        );

        assert!(err.is_pool());
        assert!(err.is_pool_limit());
        assert_eq!(
            err.pool_limit().map(PoolLimitError::kind),
            Some(PoolLimitKind::MaxActivePerHost)
        );
        assert_eq!(err.pool_limit().and_then(PoolLimitError::limit), Some(2));
    }

    #[test]
    fn non_connect_errors() {
        assert!(!Error::Timeout.is_connect());
        assert!(!Error::ReadTimeout.is_connect());
        assert!(!Error::Status(http::StatusCode::OK).is_connect());
        assert!(!Error::InvalidUrl("bad".into()).is_connect());
        assert!(!Error::Redirect("nope".into()).is_connect());
        assert!(!Error::TooManyRedirects(5).is_connect());
        assert!(!Error::HttpsOnly("http".into()).is_connect());
        assert!(!Error::InvalidHeader("bad".into()).is_connect());
        assert!(!Error::Other("misc".into()).is_connect());
    }

    #[test]
    fn display_formats() {
        assert_eq!(Error::Timeout.to_string(), "request timeout");
        assert!(Error::TooManyRedirects(10).to_string().contains("10"));
        assert!(Error::HttpsOnly("http".into()).to_string().contains("http"));
    }

    #[test]
    fn is_closed_for_io_connection_reset() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset",
        ));
        assert!(err.is_closed());
    }

    #[test]
    fn is_closed_for_io_broken_pipe() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken",
        ));
        assert!(err.is_closed());
    }

    #[test]
    fn is_closed_for_io_connection_aborted() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "aborted",
        ));
        assert!(err.is_closed());
    }

    #[test]
    fn is_closed_false_for_other_io() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ));
        assert!(!err.is_closed());
    }

    #[test]
    fn is_closed_false_for_non_io_errors() {
        assert!(!Error::Timeout.is_closed());
        assert!(!Error::ConnectTimeout.is_closed());
        assert!(!Error::ReadTimeout.is_closed());
        assert!(!Error::Status(http::StatusCode::OK).is_closed());
        assert!(!Error::InvalidUrl("bad".into()).is_closed());
        assert!(!Error::Redirect("nope".into()).is_closed());
        assert!(!Error::TooManyRedirects(5).is_closed());
        assert!(!Error::HttpsOnly("http".into()).is_closed());
        assert!(!Error::InvalidHeader("bad".into()).is_closed());
        assert!(!Error::Other("misc".into()).is_closed());
        assert!(!Error::Tls("bad cert".into()).is_closed());
    }

    #[test]
    fn send_error_accessors() {
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let err = SendError::new(Error::Timeout, uri.clone());
        assert_eq!(*err.url(), uri);
        assert!(err.is_timeout());
        assert!(!err.is_connect());
        assert!(!err.is_status());
        assert!(!err.is_redirect());
        assert_eq!(err.status(), None);
    }

    #[test]
    fn send_error_status_variant() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(Error::Status(http::StatusCode::NOT_FOUND), uri);
        assert!(err.is_status());
        assert_eq!(err.status(), Some(http::StatusCode::NOT_FOUND));
        assert!(!err.is_timeout());
    }

    #[test]
    fn send_error_connect_variant() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(Error::ConnectTimeout, uri);
        assert!(err.is_connect());
        assert!(err.is_timeout());
    }

    #[test]
    fn remote_addr_error_preserves_inner_classification() {
        let remote_addr: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ))
        .with_remote_addr(remote_addr);

        assert_eq!(err.remote_addr(), Some(remote_addr));
        assert!(err.is_connect());
        assert!(!err.is_timeout());
        assert_eq!(err.root_cause().to_string(), "refused");
    }

    #[test]
    fn send_error_remote_addr_delegates_to_inner_error() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let remote_addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let err = SendError::new(Error::ConnectTimeout.with_remote_addr(remote_addr), uri);

        assert_eq!(err.remote_addr(), Some(remote_addr));
        assert!(err.is_connect());
        assert!(err.is_timeout());
    }

    #[test]
    fn send_error_redirect_variant() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(Error::Redirect("no location".into()), uri);
        assert!(err.is_redirect());
    }

    #[test]
    fn send_error_display() {
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let err = SendError::new(Error::Timeout, uri);
        let msg = err.to_string();
        assert!(msg.contains("request timeout"));
        assert!(msg.contains("example.com"));
    }

    #[test]
    fn send_error_source() {
        use std::error::Error as StdError;
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(Error::Timeout, uri);
        assert!(err.source().is_some());
    }

    #[test]
    fn send_error_error_ref() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(Error::Timeout, uri);
        assert!(err.error().is_timeout());
    }

    #[test]
    fn send_error_into_error() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(Error::Timeout, uri);
        let inner = err.into_error();
        assert!(inner.is_timeout());
    }

    #[test]
    fn send_error_into_from() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let send_err = SendError::new(Error::ReadTimeout, uri);
        let err: Error = send_err.into();
        assert!(matches!(err, Error::ReadTimeout));
    }

    #[test]
    fn boxed_tls_error_exposes_source_chain() {
        use std::error::Error as StdError;

        let err = Error::Tls(Box::new(OuterLayer { source: InnerLayer }));
        let source = err.source().expect("TLS should expose boxed source");

        assert_eq!(source.to_string(), "outer layer");
        assert_eq!(err.root_cause().to_string(), "inner cause");
    }

    #[test]
    fn boxed_other_error_exposes_source_chain() {
        use std::error::Error as StdError;

        let err = Error::Other(Box::new(OuterLayer { source: InnerLayer }));
        let source = err.source().expect("Other should expose boxed source");

        assert_eq!(source.to_string(), "outer layer");
        assert_eq!(err.root_cause().to_string(), "inner cause");
    }

    #[test]
    fn send_error_root_cause_forwards_to_underlying_error() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(
            Error::Other(Box::new(OuterLayer { source: InnerLayer })),
            uri,
        );

        assert_eq!(err.root_cause().to_string(), "inner cause");
    }

    #[test]
    fn send_error_display_includes_hidden_root_cause_and_redacts_url() {
        let uri: Uri = "http://user:password@example.com/path".parse().unwrap();
        let err = SendError::new(Error::Tls(Box::new(OuterLayer { source: InnerLayer })), uri);

        let display = err.to_string();
        assert!(display.contains("TLS error: outer layer: inner cause"));
        assert!(display.contains("http://[redacted]@example.com/path"));
        assert!(!display.contains("user:password"));
    }

    #[test]
    fn error_from_http_error() {
        let err: Result<http::Request<()>, _> = http::Request::builder()
            .method("GET")
            .header("bad\nheader", "value")
            .body(());
        let http_err = err.unwrap_err();
        let err: Error = Error::Http(http_err);
        assert!(!err.is_closed());
    }

    #[test]
    fn display_all_variants() {
        assert!(
            Error::ConnectTimeout
                .to_string()
                .contains("connect timeout")
        );
        assert!(Error::ReadTimeout.to_string().contains("read timeout"));
        assert!(Error::InvalidUrl("bad".into()).to_string().contains("bad"));
        assert!(
            Error::InvalidHeader("hdr".into())
                .to_string()
                .contains("hdr")
        );
        assert!(Error::Tls("tls err".into()).to_string().contains("tls"));
        assert!(Error::Other("other".into()).to_string().contains("other"));
        let io_err = std::io::Error::other("io");
        assert!(Error::Io(io_err).to_string().contains("io"));
    }

    #[test]
    fn error_debug_format() {
        let err = Error::Timeout;
        let dbg = format!("{:?}", err);
        assert!(dbg.contains("Timeout"));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn is_closed_hyper_canceled() {
        // Create a duplex connection and drop server side to trigger a canceled hyper error
        use crate::runtime::tokio_rt::TokioIo;

        let (client_io, server_io) = tokio::io::duplex(1024);
        let io = TokioIo::new(client_io);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .expect("handshake");

        tokio::spawn(async move {
            let _ = conn.await;
        });

        // Drop server side to close the connection
        drop(server_io);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let req = http::Request::builder()
            .uri("http://example.com/")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let result = sender.send_request(req).await;
        assert!(result.is_err(), "request should fail after server drops");
        let hyper_err = result.unwrap_err();
        // The hyper error should be canceled or closed or incomplete
        assert!(
            hyper_err.is_canceled() || hyper_err.is_closed() || hyper_err.is_incomplete_message(),
            "expected canceled/closed/incomplete, got: {hyper_err:?}"
        );

        let err = Error::Hyper(hyper_err);
        assert!(
            err.is_closed(),
            "Error::Hyper with canceled/closed should return true from is_closed()"
        );
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn is_closed_hyper_non_canceled_returns_false() {
        // Create a hyper error that is NOT canceled/closed/incomplete
        // A parse error (sending garbage) is neither canceled nor closed
        use crate::runtime::tokio_rt::TokioIo;
        use tokio::io::AsyncWriteExt;

        let (client_io, mut server_io) = tokio::io::duplex(1024);
        let io = TokioIo::new(client_io);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .expect("handshake");

        tokio::spawn(async move {
            let _ = conn.await;
        });

        // Write garbage HTTP response to trigger a parse error
        let _ = server_io.write_all(b"NOT HTTP/1.1\r\n\r\n").await;

        let req = http::Request::builder()
            .uri("http://example.com/")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let result = sender.send_request(req).await;
        if let Err(hyper_err) = result {
            // If it's a parse error, it should NOT be is_closed
            if !hyper_err.is_canceled()
                && !hyper_err.is_closed()
                && !hyper_err.is_incomplete_message()
            {
                let err = Error::Hyper(hyper_err);
                // Check that the io source path returns false for non-matching io errors
                assert!(
                    !err.is_closed(),
                    "parse error should not be considered closed"
                );
            }
        }
    }

    #[test]
    fn is_dns_false_for_addr_not_available() {
        // AddrNotAvailable is a local address binding error (EADDRNOTAVAIL),
        // not a DNS resolution failure.
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "address not available",
        ));
        assert!(!err.is_dns());
    }

    #[test]
    fn is_dns_for_message_containing_dns() {
        let err = Error::Io(std::io::Error::other("dns error"));
        assert!(err.is_dns());
    }

    #[test]
    fn is_dns_for_message_containing_resolve() {
        let err = Error::Io(std::io::Error::other("failed to resolve host"));
        assert!(err.is_dns());
    }

    #[test]
    fn is_dns_for_no_dns_resolver() {
        let err = Error::InvalidUrl("no DNS resolver configured".into());
        assert!(err.is_dns());
    }

    #[test]
    fn is_dns_false_for_non_io_errors() {
        assert!(!Error::Timeout.is_dns());
        assert!(!Error::ConnectTimeout.is_dns());
        assert!(!Error::ReadTimeout.is_dns());
        assert!(!Error::Status(http::StatusCode::OK).is_dns());
        assert!(!Error::Tls("bad".into()).is_dns());
        assert!(!Error::Redirect("nope".into()).is_dns());
        assert!(!Error::TooManyRedirects(5).is_dns());
    }

    #[test]
    fn send_error_is_dns_for_os_error() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        // Linux glibc getaddrinfo failure message
        let err = SendError::new(
            Error::Io(std::io::Error::other(
                "failed to lookup address information: Name or service not known",
            )),
            uri,
        );
        assert!(err.is_dns());
    }

    #[test]
    fn send_error_is_dns_false() {
        let uri: Uri = "http://example.com/".parse().unwrap();
        let err = SendError::new(Error::Timeout, uri);
        assert!(!err.is_dns());
    }

    #[test]
    fn is_dns_false_for_connection_refused() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        assert!(!err.is_dns());
    }

    #[test]
    fn is_closed_for_connection_refused() {
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        // Connection refused means the connection was never established,
        // so is_closed should return false (it's not a "closed" reused connection).
        assert!(!err.is_closed());
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn is_connect_for_hyper_connection_error() {
        // Create a custom IO that fails with ConnectionRefused on read/write.
        // The handshake itself returns immediately; the error surfaces when we
        // drive the connection or send a request.
        use crate::runtime::tokio_rt::TokioIo;
        use std::io;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct FailingIo;

        impl tokio::io::AsyncRead for FailingIo {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "connection refused",
                )))
            }
        }

        impl tokio::io::AsyncWrite for FailingIo {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "connection refused",
                )))
            }

            fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        let io = TokioIo::new(FailingIo);
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake::<_, http_body_util::Empty<bytes::Bytes>>(io)
                .await
                .expect("handshake future should succeed (lazy)");

        // Drive the connection. The first read/write will hit our failing IO
        // and produce a hyper error wrapping ConnectionRefused.
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let req = http::Request::builder()
            .uri("http://example.com/")
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .unwrap();

        let result = sender.send_request(req).await;
        match result {
            Err(hyper_err) => {
                let err = Error::Hyper(hyper_err);
                assert!(
                    err.is_connect(),
                    "Error::Hyper wrapping a connection error should return true from is_connect()"
                );
            }
            Ok(_) => panic!("expected send_request to fail on failing IO"),
        }
    }

    #[test]
    fn is_dns_for_invalid_url_cannot_resolve() {
        let err = Error::InvalidUrl("cannot resolve host.invalid:80: dns error".into());
        assert!(
            err.is_dns(),
            "Error::InvalidUrl with 'cannot resolve' should match is_dns()"
        );
    }

    #[test]
    fn is_dns_for_invalid_url_no_dns_resolver() {
        let err = Error::InvalidUrl("no DNS resolver configured for host:80".into());
        assert!(
            err.is_dns(),
            "Error::InvalidUrl with 'no DNS resolver' should match is_dns()"
        );
    }

    #[test]
    fn is_dns_for_invalid_url_unrelated() {
        let err = Error::InvalidUrl("bad url format".into());
        assert!(
            !err.is_dns(),
            "Error::InvalidUrl without DNS keywords should not match is_dns()"
        );
    }
}
