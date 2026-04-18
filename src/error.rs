use bytes::Bytes;
use http_body_util::combinators::BoxBody;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

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

pub type Result<T> = std::result::Result<T, Error>;
pub type HyperBody = BoxBody<Bytes, Error>;
