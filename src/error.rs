use bytes::Bytes;
use http_body_util::combinators::BoxBody;

/// Boxed error type for dynamic dispatch.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Errors that can occur during HTTP operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),

    #[error("hyper error: {0}")]
    Hyper(#[from] hyper::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS error: {0}")]
    Tls(BoxError),

    #[error("connection pool error: {0}")]
    Pool(String),

    #[error("request timeout")]
    Timeout,

    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("{0}")]
    Other(BoxError),
}

/// Result type for aioduct operations.
pub type Result<T> = std::result::Result<T, Error>;
/// Boxed HTTP body type used throughout aioduct.
pub type HyperBody = BoxBody<Bytes, Error>;
