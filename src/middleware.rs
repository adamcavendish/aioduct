use std::sync::Arc;

use http::Uri;

use crate::error::HyperBody;

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
}
