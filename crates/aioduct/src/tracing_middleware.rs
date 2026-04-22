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

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};

    fn empty_body() -> AioductBody {
        http_body_util::Full::new(bytes::Bytes::new())
            .map_err(|never| match never {})
            .boxed()
    }

    struct EventCollector(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for EventCollector {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            use tracing::field::Visit;
            struct Visitor(String);
            impl Visit for Visitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    self.0.push_str(&format!("{}={:?} ", field.name(), value));
                }
            }
            let mut v = Visitor(String::new());
            event.record(&mut v);
            self.0.lock().unwrap().push(v.0);
        }
    }

    fn setup_collector() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<String>>>) {
        use tracing_subscriber::layer::SubscriberExt;
        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(EventCollector(events.clone()));
        let guard = tracing::subscriber::set_default(subscriber);
        (guard, events)
    }

    #[test]
    fn new_and_default() {
        let _m = TracingMiddleware::new();
        let _d = TracingMiddleware;
    }

    #[test]
    fn on_request_emits_event() {
        let (_guard, events) = setup_collector();
        let m = TracingMiddleware::new();
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut req = http::Request::get("http://example.com/path")
            .body(empty_body())
            .unwrap();
        m.on_request(&mut req, &uri);
        let captured = events.lock().unwrap();
        assert!(!captured.is_empty());
        assert!(captured[0].contains("http.request.start"));
    }

    #[test]
    fn on_response_emits_event() {
        let (_guard, events) = setup_collector();
        let m = TracingMiddleware::new();
        let uri: Uri = "http://example.com/path".parse().unwrap();
        let mut resp = http::Response::builder()
            .status(200)
            .body(empty_body())
            .unwrap();
        m.on_response(&mut resp, &uri);
        let captured = events.lock().unwrap();
        assert!(!captured.is_empty());
        assert!(captured[0].contains("http.request.done"));
    }

    #[test]
    fn on_error_emits_event() {
        let (_guard, events) = setup_collector();
        let m = TracingMiddleware::new();
        let uri: Uri = "http://example.com/".parse().unwrap();
        m.on_error(&Error::Timeout, &uri, &Method::GET);
        let captured = events.lock().unwrap();
        assert!(!captured.is_empty());
        assert!(captured[0].contains("http.request.error"));
    }

    #[test]
    fn on_redirect_emits_event() {
        let (_guard, events) = setup_collector();
        let m = TracingMiddleware::new();
        let from: Uri = "http://a.com/".parse().unwrap();
        let to: Uri = "http://b.com/".parse().unwrap();
        m.on_redirect(StatusCode::MOVED_PERMANENTLY, &from, &to);
        let captured = events.lock().unwrap();
        assert!(!captured.is_empty());
        assert!(captured[0].contains("http.redirect"));
    }

    #[test]
    fn on_retry_emits_event() {
        let (_guard, events) = setup_collector();
        let m = TracingMiddleware::new();
        let uri: Uri = "http://example.com/".parse().unwrap();
        m.on_retry(&Error::Timeout, &uri, &Method::POST, 2);
        let captured = events.lock().unwrap();
        assert!(!captured.is_empty());
        assert!(captured[0].contains("http.retry"));
    }
}
