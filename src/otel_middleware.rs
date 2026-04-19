use std::borrow::Cow;
use std::sync::Arc;

use http::{Method, StatusCode, Uri};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use opentelemetry_http::HeaderInjector;

use crate::error::{Error, HyperBody};
use crate::middleware::Middleware;

/// OpenTelemetry middleware that injects W3C trace context into outgoing requests
/// and records HTTP semantic convention attributes.
///
/// By default, uses the global propagator from `opentelemetry::global`. To use
/// an explicit propagator, call [`OtelMiddleware::with_propagator`].
pub struct OtelMiddleware {
    propagator: Option<Arc<dyn TextMapPropagator + Send + Sync>>,
}

impl OtelMiddleware {
    /// Create a new OTel middleware using the global text map propagator.
    pub fn new() -> Self {
        Self { propagator: None }
    }

    /// Create a new OTel middleware with an explicit propagator.
    pub fn with_propagator(propagator: impl TextMapPropagator + Send + Sync + 'static) -> Self {
        Self {
            propagator: Some(Arc::new(propagator)),
        }
    }

    fn inject_context(&self, headers: &mut http::HeaderMap) {
        let cx = Context::current();
        let mut injector = HeaderInjector(headers);
        match &self.propagator {
            Some(p) => p.inject_context(&cx, &mut injector),
            None => opentelemetry::global::get_text_map_propagator(|p| {
                p.inject_context(&cx, &mut injector);
            }),
        }
    }
}

impl Default for OtelMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for OtelMiddleware {
    fn on_request(&self, request: &mut http::Request<HyperBody>, uri: &Uri) {
        let tracer = opentelemetry::global::tracer("aioduct");
        let method = request.method().as_str().to_owned();
        let url = uri.to_string();

        let span = tracer
            .span_builder(format!("HTTP {method}"))
            .with_kind(SpanKind::Client)
            .with_attributes(vec![
                KeyValue::new("http.request.method", method),
                KeyValue::new("url.full", url),
            ])
            .start(&tracer);

        let cx = Context::current_with_span(span);
        let _guard = cx.attach();

        self.inject_context(request.headers_mut());
    }

    fn on_response(&self, response: &mut http::Response<HyperBody>, _uri: &Uri) {
        let cx = Context::current();
        let span = cx.span();
        let status = response.status().as_u16() as i64;
        span.set_attribute(KeyValue::new("http.response.status_code", status));

        if response.status().is_server_error() {
            span.set_status(Status::Error {
                description: Cow::Owned(response.status().to_string()),
            });
        }

        span.end();
    }

    fn on_error(&self, error: &Error, _uri: &Uri, _method: &Method) {
        let cx = Context::current();
        let span = cx.span();
        span.set_status(Status::Error {
            description: Cow::Owned(error.to_string()),
        });
        span.set_attribute(KeyValue::new("error.type", error_type(error)));
        span.end();
    }

    fn on_redirect(&self, status: StatusCode, _from: &Uri, to: &Uri) {
        let cx = Context::current();
        let span = cx.span();
        span.add_event(
            "http.redirect",
            vec![
                KeyValue::new("http.response.status_code", status.as_u16() as i64),
                KeyValue::new("http.redirect.target", to.to_string()),
            ],
        );
    }

    fn on_retry(&self, error: &Error, _uri: &Uri, _method: &Method, attempt: u32) {
        let cx = Context::current();
        let span = cx.span();
        span.add_event(
            "http.retry",
            vec![
                KeyValue::new("http.retry.attempt", attempt as i64),
                KeyValue::new("error.type", error_type(error)),
            ],
        );
    }
}

fn error_type(error: &Error) -> &'static str {
    match error {
        Error::Timeout => "timeout",
        Error::Io(_) => "io",
        Error::Hyper(_) => "hyper",
        Error::Tls(_) => "tls",
        Error::Status(_) => "status",
        Error::InvalidUrl(_) => "invalid_url",
        _ => "other",
    }
}
