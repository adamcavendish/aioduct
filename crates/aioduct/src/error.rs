use http::Uri;

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
        write!(f, "{} for url ({})", self.error, self.url)
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
}

impl From<SendError> for Error {
    fn from(e: SendError) -> Self {
        e.error
    }
}

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

    /// Returns `true` if the error indicates a reused connection was closed by the peer.
    ///
    /// This covers both TCP-level closes (RST, FIN) and HTTP-level closes
    /// (GOAWAY, canceled requests). Useful for distinguishing "stale pool
    /// connection" errors from genuine server-side failures.
    pub fn is_closed(&self) -> bool {
        use std::error::Error as _;
        match self {
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
