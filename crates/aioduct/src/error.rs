use bytes::Bytes;
use http_body_util::combinators::BoxBody;

/// Boxed error type for dynamic dispatch.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Errors that can occur during HTTP operations.
#[derive(Debug, thiserror::Error)]
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
    Tls(BoxError),

    /// The request timed out.
    #[error("request timeout")]
    Timeout,

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

    /// A catch-all for other errors.
    #[error("{0}")]
    Other(BoxError),
}

/// Boxed HTTP body type used throughout aioduct.
pub type AioductBody = BoxBody<Bytes, Error>;
