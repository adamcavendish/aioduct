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

    /// A connection pool error.
    #[error("connection pool error: {0}")]
    Pool(String),

    /// The request timed out.
    #[error("request timeout")]
    Timeout,

    /// The URL is invalid or cannot be resolved.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// The response had a 4xx or 5xx status code.
    #[error("HTTP status error: {0}")]
    Status(http::StatusCode),

    /// A catch-all for other errors.
    #[error("{0}")]
    Other(BoxError),
}

/// Boxed HTTP body type used throughout aioduct.
pub type HyperBody = BoxBody<Bytes, Error>;
