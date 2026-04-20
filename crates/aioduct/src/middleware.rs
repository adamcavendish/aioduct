use std::sync::Arc;

use http::{Method, StatusCode, Uri};

use crate::error::{AioductBody, Error};

/// Middleware that can inspect or modify requests and responses.
///
/// Implement this trait to add cross-cutting behavior like logging, metrics,
/// or auth token refresh. Middleware is applied in order: request hooks run
/// first-to-last, response hooks run last-to-first.
///
/// Note: all hooks are synchronous. Async operations (e.g., token refresh) are not supported.
pub trait Middleware: Send + Sync + 'static {
    /// Called before the request is sent. May modify the request in place.
    fn on_request(&self, request: &mut http::Request<AioductBody>, uri: &Uri) {
        let _ = (request, uri);
    }

    /// Called after the response is received. May modify the response in place.
    fn on_response(&self, response: &mut http::Response<AioductBody>, uri: &Uri) {
        let _ = (response, uri);
    }

    /// Called when a request fails with an error.
    fn on_error(&self, error: &Error, uri: &Uri, method: &Method) {
        let _ = (error, uri, method);
    }

    /// Called when a redirect is followed.
    fn on_redirect(&self, status: StatusCode, from: &Uri, to: &Uri) {
        let _ = (status, from, to);
    }

    /// Called before a retry attempt.
    fn on_retry(&self, error: &Error, uri: &Uri, method: &Method, attempt: u32) {
        let _ = (error, uri, method, attempt);
    }
}

impl<F> Middleware for F
where
    F: Fn(&mut http::Request<AioductBody>, &Uri) + Send + Sync + 'static,
{
    fn on_request(&self, request: &mut http::Request<AioductBody>, uri: &Uri) {
        (self)(request, uri);
    }
}

pub(crate) struct MiddlewareStack {
    layers: Vec<Arc<dyn Middleware>>,
}

impl Clone for MiddlewareStack {
    fn clone(&self) -> Self {
        Self {
            layers: self.layers.clone(),
        }
    }
}

impl MiddlewareStack {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    pub fn push(&mut self, middleware: Arc<dyn Middleware>) {
        self.layers.push(middleware);
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn apply_request(&self, request: &mut http::Request<AioductBody>, uri: &Uri) {
        for layer in &self.layers {
            layer.on_request(request, uri);
        }
    }

    pub fn apply_response(&self, response: &mut http::Response<AioductBody>, uri: &Uri) {
        for layer in self.layers.iter().rev() {
            layer.on_response(response, uri);
        }
    }

    pub fn apply_error(&self, error: &Error, uri: &Uri, method: &Method) {
        for layer in &self.layers {
            layer.on_error(error, uri, method);
        }
    }

    pub fn apply_redirect(&self, status: StatusCode, from: &Uri, to: &Uri) {
        for layer in &self.layers {
            layer.on_redirect(status, from, to);
        }
    }

    pub fn apply_retry(&self, error: &Error, uri: &Uri, method: &Method, attempt: u32) {
        for layer in &self.layers {
            layer.on_retry(error, uri, method, attempt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::sync::Mutex;

    fn empty_body() -> AioductBody {
        http_body_util::Full::new(bytes::Bytes::new())
            .map_err(|never| match never {})
            .boxed()
    }

    fn test_uri() -> Uri {
        "http://example.com/test".parse().unwrap()
    }

    struct RecordingMiddleware {
        id: i32,
        log: Arc<Mutex<Vec<(i32, &'static str)>>>,
    }

    impl Middleware for RecordingMiddleware {
        fn on_request(&self, _req: &mut http::Request<AioductBody>, _uri: &Uri) {
            self.log.lock().unwrap().push((self.id, "request"));
        }
        fn on_response(&self, _resp: &mut http::Response<AioductBody>, _uri: &Uri) {
            self.log.lock().unwrap().push((self.id, "response"));
        }
        fn on_error(&self, _err: &Error, _uri: &Uri, _method: &Method) {
            self.log.lock().unwrap().push((self.id, "error"));
        }
        fn on_redirect(&self, _status: StatusCode, _from: &Uri, _to: &Uri) {
            self.log.lock().unwrap().push((self.id, "redirect"));
        }
        fn on_retry(&self, _err: &Error, _uri: &Uri, _method: &Method, _attempt: u32) {
            self.log.lock().unwrap().push((self.id, "retry"));
        }
    }

    fn make_stack(log: &Arc<Mutex<Vec<(i32, &'static str)>>>) -> MiddlewareStack {
        let mut stack = MiddlewareStack::new();
        stack.push(Arc::new(RecordingMiddleware {
            id: 1,
            log: Arc::clone(log),
        }));
        stack.push(Arc::new(RecordingMiddleware {
            id: 2,
            log: Arc::clone(log),
        }));
        stack
    }

    #[test]
    fn new_stack_is_empty() {
        let stack = MiddlewareStack::new();
        assert!(stack.is_empty());
    }

    #[test]
    fn push_makes_non_empty() {
        let mut stack = MiddlewareStack::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        stack.push(Arc::new(RecordingMiddleware {
            id: 1,
            log: Arc::clone(&log),
        }));
        assert!(!stack.is_empty());
    }

    #[test]
    fn apply_request_runs_first_to_last() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stack = make_stack(&log);
        let uri = test_uri();
        let mut req = http::Request::get("http://example.com")
            .body(empty_body())
            .unwrap();
        stack.apply_request(&mut req, &uri);
        let entries = log.lock().unwrap();
        assert_eq!(entries[0], (1, "request"));
        assert_eq!(entries[1], (2, "request"));
    }

    #[test]
    fn apply_response_runs_last_to_first() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stack = make_stack(&log);
        let uri = test_uri();
        let mut resp = http::Response::builder()
            .status(200)
            .body(empty_body())
            .unwrap();
        stack.apply_response(&mut resp, &uri);
        let entries = log.lock().unwrap();
        assert_eq!(entries[0], (2, "response"));
        assert_eq!(entries[1], (1, "response"));
    }

    #[test]
    fn apply_error_invokes_all() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stack = make_stack(&log);
        let uri = test_uri();
        stack.apply_error(&Error::Timeout, &uri, &Method::GET);
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(_, kind)| *kind == "error"));
    }

    #[test]
    fn apply_redirect_invokes_all() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stack = make_stack(&log);
        let from: Uri = "http://a.com".parse().unwrap();
        let to: Uri = "http://b.com".parse().unwrap();
        stack.apply_redirect(StatusCode::MOVED_PERMANENTLY, &from, &to);
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(_, kind)| *kind == "redirect"));
    }

    #[test]
    fn apply_retry_invokes_all() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stack = make_stack(&log);
        let uri = test_uri();
        stack.apply_retry(&Error::Timeout, &uri, &Method::POST, 1);
        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|(_, kind)| *kind == "retry"));
    }

    #[test]
    fn closure_as_middleware() {
        let mut stack = MiddlewareStack::new();
        stack.push(Arc::new(
            |req: &mut http::Request<AioductBody>, _uri: &Uri| {
                req.headers_mut()
                    .insert("x-test", http::header::HeaderValue::from_static("added"));
            },
        ));
        let uri = test_uri();
        let mut req = http::Request::get("http://example.com")
            .body(empty_body())
            .unwrap();
        stack.apply_request(&mut req, &uri);
        assert_eq!(req.headers().get("x-test").unwrap(), "added");
    }

    #[test]
    fn clone_preserves_layers() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stack = make_stack(&log);
        let cloned = stack.clone();
        assert!(!cloned.is_empty());
    }

    #[test]
    fn empty_stack_apply_request_no_panic() {
        let stack = MiddlewareStack::new();
        let uri = test_uri();
        let mut req = http::Request::get("http://example.com")
            .body(empty_body())
            .unwrap();
        stack.apply_request(&mut req, &uri);
    }

    #[test]
    fn empty_stack_apply_response_no_panic() {
        let stack = MiddlewareStack::new();
        let uri = test_uri();
        let mut resp = http::Response::builder()
            .status(200)
            .body(empty_body())
            .unwrap();
        stack.apply_response(&mut resp, &uri);
    }

    #[test]
    fn empty_stack_apply_error_no_panic() {
        let stack = MiddlewareStack::new();
        stack.apply_error(&Error::Timeout, &test_uri(), &Method::GET);
    }

    #[test]
    fn empty_stack_apply_redirect_no_panic() {
        let stack = MiddlewareStack::new();
        let from: Uri = "http://a.com".parse().unwrap();
        let to: Uri = "http://b.com".parse().unwrap();
        stack.apply_redirect(StatusCode::MOVED_PERMANENTLY, &from, &to);
    }

    #[test]
    fn empty_stack_apply_retry_no_panic() {
        let stack = MiddlewareStack::new();
        stack.apply_retry(&Error::Timeout, &test_uri(), &Method::GET, 1);
    }

    #[test]
    fn closure_middleware_default_hooks_no_panic() {
        let mut stack = MiddlewareStack::new();
        stack.push(Arc::new(
            |_req: &mut http::Request<AioductBody>, _uri: &Uri| {},
        ));
        let uri = test_uri();
        let mut resp = http::Response::builder()
            .status(200)
            .body(empty_body())
            .unwrap();
        stack.apply_response(&mut resp, &uri);
        stack.apply_error(&Error::Timeout, &uri, &Method::GET);
        let to: Uri = "http://b.com".parse().unwrap();
        stack.apply_redirect(StatusCode::FOUND, &uri, &to);
        stack.apply_retry(&Error::Timeout, &uri, &Method::POST, 2);
    }

    #[test]
    fn default_trait_methods_no_panic() {
        struct NoopMiddleware;
        impl Middleware for NoopMiddleware {}

        let mut stack = MiddlewareStack::new();
        stack.push(Arc::new(NoopMiddleware));
        let uri = test_uri();
        let mut req = http::Request::get("http://example.com")
            .body(empty_body())
            .unwrap();
        stack.apply_request(&mut req, &uri);
        let mut resp = http::Response::builder()
            .status(200)
            .body(empty_body())
            .unwrap();
        stack.apply_response(&mut resp, &uri);
        stack.apply_error(&Error::Timeout, &uri, &Method::GET);
        let to: Uri = "http://b.com".parse().unwrap();
        stack.apply_redirect(StatusCode::FOUND, &uri, &to);
        stack.apply_retry(&Error::Timeout, &uri, &Method::POST, 1);
    }
}
