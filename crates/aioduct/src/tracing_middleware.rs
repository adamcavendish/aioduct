use http::{Method, StatusCode, Uri};

use crate::error::{AioductBody, Error};
use crate::middleware::Middleware;

/// Middleware that emits `tracing` events for HTTP request lifecycle.
///
/// All spans and events use `debug` or `trace` level only — this is a library,
/// so `info` and above are reserved for the application.
///
/// - `on_request` / `on_response`: `debug` level
/// - `on_error`, `on_redirect`, `on_retry`: `debug` level
pub struct TracingMiddleware;

impl TracingMiddleware {
    /// Create a new tracing middleware.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TracingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for TracingMiddleware {
    fn on_request(&self, request: &mut http::Request<AioductBody>, uri: &Uri) {
        tracing::debug!(
            method = %request.method(),
            uri = %uri,
            "http.request.start",
        );
    }

    fn on_response(&self, response: &mut http::Response<AioductBody>, uri: &Uri) {
        tracing::debug!(
            status = response.status().as_u16(),
            uri = %uri,
            "http.request.done",
        );
    }

    fn on_error(&self, error: &Error, uri: &Uri, method: &Method) {
        tracing::debug!(
            error = %error,
            method = %method,
            uri = %uri,
            "http.request.error",
        );
    }

    fn on_redirect(&self, status: StatusCode, from: &Uri, to: &Uri) {
        tracing::debug!(
            status = status.as_u16(),
            from = %from,
            to = %to,
            "http.redirect",
        );
    }

    fn on_retry(&self, error: &Error, uri: &Uri, method: &Method, attempt: u32) {
        tracing::debug!(
            error = %error,
            method = %method,
            uri = %uri,
            attempt = attempt,
            "http.retry",
        );
    }
}
