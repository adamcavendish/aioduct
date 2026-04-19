use std::borrow::Cow;
use std::sync::Arc;

use http::{Method, StatusCode, Uri};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::{Status, TraceContextExt};
use opentelemetry::{Context, KeyValue};
use opentelemetry_http::HeaderInjector;

use crate::error::{Error, HyperBody};
use crate::middleware::Middleware;

/// OpenTelemetry middleware that propagates trace context and records HTTP
/// semantic convention attributes on the **current** span.
///
/// This middleware does **not** create or end spans — that is the caller's
/// responsibility. Typical usage:
///
/// ```ignore
/// use opentelemetry::trace::{Tracer, SpanKind};
///
/// let tracer = opentelemetry::global::tracer("my-app");
/// let span = tracer
///     .span_builder("HTTP GET /api")
///     .with_kind(SpanKind::Client)
///     .start(&tracer);
/// let _guard = opentelemetry::trace::mark_span_as_active(span);
///
/// let resp = client.get("https://example.com/api")?.send().await?;
/// // span is ended when `_guard` drops
/// ```
///
/// The middleware will:
/// - Inject W3C trace context headers (`traceparent`, `tracestate`) into
///   outgoing requests from the currently active span.
/// - Record `http.request.method`, `url.full`, `http.response.status_code`
///   and error attributes on the current span.
///
/// By default, uses the global propagator. Call [`OtelMiddleware::with_propagator`]
/// to supply an explicit one.
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
        self.inject_context(request.headers_mut());

        Context::map_current(|cx| {
            let span = cx.span();
            span.set_attribute(KeyValue::new(
                "http.request.method",
                request.method().as_str().to_owned(),
            ));
            span.set_attribute(KeyValue::new("url.full", uri.to_string()));
        });
    }

    fn on_response(&self, response: &mut http::Response<HyperBody>, _uri: &Uri) {
        Context::map_current(|cx| {
            let span = cx.span();
            span.set_attribute(KeyValue::new(
                "http.response.status_code",
                response.status().as_u16() as i64,
            ));

            if response.status().is_server_error() {
                span.set_status(Status::Error {
                    description: Cow::Owned(response.status().to_string()),
                });
            }
        });
    }

    fn on_error(&self, error: &Error, _uri: &Uri, _method: &Method) {
        Context::map_current(|cx| {
            let span = cx.span();
            span.set_status(Status::Error {
                description: Cow::Owned(error.to_string()),
            });
            span.set_attribute(KeyValue::new("error.type", error_type(error)));
        });
    }

    fn on_redirect(&self, status: StatusCode, _from: &Uri, to: &Uri) {
        Context::map_current(|cx| {
            cx.span().add_event(
                "http.redirect",
                vec![
                    KeyValue::new("http.response.status_code", status.as_u16() as i64),
                    KeyValue::new("http.redirect.target", to.to_string()),
                ],
            );
        });
    }

    fn on_retry(&self, error: &Error, _uri: &Uri, _method: &Method, attempt: u32) {
        Context::map_current(|cx| {
            cx.span().add_event(
                "http.retry",
                vec![
                    KeyValue::new("http.retry.attempt", attempt as i64),
                    KeyValue::new("error.type", error_type(error)),
                ],
            );
        });
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
