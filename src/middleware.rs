use std::sync::Arc;

use http::{Method, StatusCode, Uri};

use crate::error::{Error, HyperBody};

/// Middleware that can inspect or modify requests and responses.
///
/// Implement this trait to add cross-cutting behavior like logging, metrics,
/// or auth token refresh. Middleware is applied in order: request hooks run
/// first-to-last, response hooks run last-to-first.
pub trait Middleware: Send + Sync + 'static {
    /// Called before the request is sent. May modify the request in place.
    fn on_request(&self, request: &mut http::Request<HyperBody>, uri: &Uri) {
        let _ = (request, uri);
    }

    /// Called after the response is received. May modify the response in place.
    fn on_response(&self, response: &mut http::Response<HyperBody>, uri: &Uri) {
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
    F: Fn(&mut http::Request<HyperBody>, &Uri) + Send + Sync + 'static,
{
    fn on_request(&self, request: &mut http::Request<HyperBody>, uri: &Uri) {
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

    pub fn apply_request(&self, request: &mut http::Request<HyperBody>, uri: &Uri) {
        for layer in &self.layers {
            layer.on_request(request, uri);
        }
    }

    pub fn apply_response(&self, response: &mut http::Response<HyperBody>, uri: &Uri) {
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
