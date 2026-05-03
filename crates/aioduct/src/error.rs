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

impl Error {
    /// Returns `true` if the error is a network-level failure (I/O, TLS, timeout).
    pub fn is_connect(&self) -> bool {
        matches!(self, Error::Io(_) | Error::Tls(_) | Error::Timeout)
    }

    /// Returns `true` if the error is a timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout)
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn is_connect_for_timeout() {
        let err = Error::Timeout;
        assert!(err.is_connect());
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
    fn non_connect_errors() {
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
}
