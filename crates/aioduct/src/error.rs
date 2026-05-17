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

    /// The connection attempt timed out.
    #[error("connect timeout")]
    ConnectTimeout,

    /// Reading the response timed out.
    #[error("read timeout")]
    ReadTimeout,

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
        write!(f, "{} for url ({})", self.error, redact_url(&self.url))
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
        matches!(self, Error::Io(_) | Error::Tls(_) | Error::ConnectTimeout)
    }

    /// Returns `true` if the error is a timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Error::Timeout | Error::ConnectTimeout | Error::ReadTimeout
        )
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
}
