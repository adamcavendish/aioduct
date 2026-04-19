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

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};

    fn empty_body() -> HyperBody {
        http_body_util::Full::new(bytes::Bytes::new())
            .map_err(|never| match never {})
            .boxed()
    }

    fn test_uri() -> Uri {
        "http://example.com/api".parse().unwrap()
    }

    #[derive(Debug)]
    struct TestPropagator {
        header_name: &'static str,
        header_value: &'static str,
    }

    impl TextMapPropagator for TestPropagator {
        fn inject_context(&self, _cx: &opentelemetry::Context, injector: &mut dyn Injector) {
            injector.set(self.header_name, self.header_value.to_string());
        }

        fn extract_with_context(
            &self,
            _cx: &opentelemetry::Context,
            _extractor: &dyn Extractor,
        ) -> opentelemetry::Context {
            opentelemetry::Context::new()
        }

        fn fields(&self) -> opentelemetry::propagation::text_map_propagator::FieldIter<'_> {
            opentelemetry::propagation::text_map_propagator::FieldIter::new(&[])
        }
    }

    #[test]
    fn explicit_propagator_injects_headers() {
        let mw = OtelMiddleware::with_propagator(TestPropagator {
            header_name: "x-trace-test",
            header_value: "injected-value",
        });

        let uri = test_uri();
        let mut request = http::Request::get("http://example.com/api")
            .body(empty_body())
            .unwrap();

        mw.on_request(&mut request, &uri);

        assert_eq!(
            request.headers().get("x-trace-test").unwrap(),
            "injected-value"
        );
    }

    #[test]
    fn global_propagator_fallback_does_not_panic() {
        let mw = OtelMiddleware::new();
        let uri = test_uri();
        let mut request = http::Request::get("http://example.com/api")
            .body(empty_body())
            .unwrap();

        mw.on_request(&mut request, &uri);
    }

    #[test]
    fn on_response_does_not_panic_without_active_span() {
        let mw = OtelMiddleware::new();
        let uri = test_uri();
        let mut response = http::Response::builder()
            .status(200)
            .body(empty_body())
            .unwrap();

        mw.on_response(&mut response, &uri);
    }

    #[test]
    fn on_response_server_error_does_not_panic() {
        let mw = OtelMiddleware::new();
        let uri = test_uri();
        let mut response = http::Response::builder()
            .status(503)
            .body(empty_body())
            .unwrap();

        mw.on_response(&mut response, &uri);
    }

    #[test]
    fn on_error_records_without_panic() {
        let mw = OtelMiddleware::new();
        let uri = test_uri();
        let error = Error::Timeout;

        mw.on_error(&error, &uri, &Method::GET);
    }

    #[test]
    fn on_redirect_records_without_panic() {
        let from: Uri = "http://old.example.com/a".parse().unwrap();
        let to: Uri = "http://new.example.com/b".parse().unwrap();
        let mw = OtelMiddleware::new();

        mw.on_redirect(StatusCode::MOVED_PERMANENTLY, &from, &to);
    }

    #[test]
    fn on_retry_records_without_panic() {
        let mw = OtelMiddleware::new();
        let uri = test_uri();
        let error = Error::Timeout;

        mw.on_retry(&error, &uri, &Method::GET, 2);
    }

    #[test]
    fn error_type_mapping() {
        assert_eq!(error_type(&Error::Timeout), "timeout");
        assert_eq!(error_type(&Error::Io(std::io::Error::other("test"))), "io");
        assert_eq!(error_type(&Error::InvalidUrl("bad".into())), "invalid_url");
        assert_eq!(error_type(&Error::Status(StatusCode::NOT_FOUND)), "status");
        assert_eq!(error_type(&Error::Tls("tls fail".into())), "tls");
        assert_eq!(error_type(&Error::Other("something else".into())), "other");
    }

    #[test]
    fn with_propagator_uses_explicit_not_global() {
        let mw = OtelMiddleware::with_propagator(TestPropagator {
            header_name: "x-custom-trace",
            header_value: "from-explicit",
        });

        opentelemetry::global::set_text_map_propagator(TestPropagator {
            header_name: "x-global-trace",
            header_value: "from-global",
        });

        let uri = test_uri();
        let mut request = http::Request::get("http://example.com/api")
            .body(empty_body())
            .unwrap();

        mw.on_request(&mut request, &uri);

        assert_eq!(
            request.headers().get("x-custom-trace").unwrap(),
            "from-explicit",
        );
        assert!(request.headers().get("x-global-trace").is_none());
    }

    #[test]
    fn default_impl_is_same_as_new() {
        let _mw: OtelMiddleware = OtelMiddleware::default();
    }
}
