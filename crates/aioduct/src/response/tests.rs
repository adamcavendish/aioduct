use super::*;
use http_body_util::BodyExt;

fn empty_body() -> ResponseBodySend {
    ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::new())
            .map_err(|never| match never {})
            .boxed_unsync(),
    )
}

fn make_response(status: u16) -> Response {
    let inner = http::Response::builder()
        .status(status)
        .body(empty_body())
        .unwrap();
    Response::new(inner, "http://example.com".parse().unwrap())
}

#[test]
fn status_returns_correct_code() {
    let resp = make_response(200);
    assert_eq!(resp.status(), StatusCode::OK);
}

#[test]
fn url_returns_request_uri() {
    let resp = make_response(200);
    assert_eq!(resp.url().to_string(), "http://example.com/");
}

#[test]
fn error_for_status_ok_on_2xx() {
    let resp = make_response(200);
    assert!(resp.error_for_status().is_ok());
}

#[test]
fn error_for_status_err_on_4xx() {
    let resp = make_response(404);
    let err = resp.error_for_status().unwrap_err();
    match err {
        Error::Status(s) => assert_eq!(s, StatusCode::NOT_FOUND),
        _ => panic!("expected Error::Status"),
    }
}

#[test]
fn error_for_status_err_on_5xx() {
    let resp = make_response(500);
    assert!(resp.error_for_status().is_err());
}

#[test]
fn error_for_status_ref_ok_on_2xx() {
    let resp = make_response(200);
    assert!(resp.error_for_status_ref().is_ok());
}

#[test]
fn error_for_status_ref_err_on_4xx() {
    let resp = make_response(403);
    assert!(resp.error_for_status_ref().is_err());
}

#[test]
fn content_length_present() {
    let inner = http::Response::builder()
        .header("Content-Length", "42")
        .body(empty_body())
        .unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    assert_eq!(resp.content_length(), Some(42));
}

#[test]
fn content_length_missing() {
    let resp = make_response(200);
    assert_eq!(resp.content_length(), None);
}

#[test]
fn content_length_non_numeric() {
    let inner = http::Response::builder()
        .header("Content-Length", "abc")
        .body(empty_body())
        .unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    assert_eq!(resp.content_length(), None);
}

#[test]
fn remote_addr_initially_none() {
    let resp = make_response(200);
    assert!(resp.remote_addr().is_none());
}

#[test]
fn remote_addr_set_and_get() {
    let mut resp = make_response(200);
    let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
    resp.set_remote_addr(Some(addr));
    assert_eq!(resp.remote_addr(), Some(addr));
}

#[test]
fn version_returns_http_version() {
    let resp = make_response(200);
    assert_eq!(resp.version(), Version::HTTP_11);
}

#[test]
fn headers_mut_allows_modification() {
    let mut resp = make_response(200);
    resp.headers_mut()
        .insert("x-test", "value".parse().unwrap());
    assert_eq!(resp.headers().get("x-test").unwrap(), "value");
}

#[test]
fn extensions_insert_and_read() {
    let mut resp = make_response(200);
    resp.extensions_mut().insert(42u32);
    assert_eq!(resp.extensions().get::<u32>(), Some(&42));
}

#[test]
fn debug_format() {
    let resp = make_response(200);
    let dbg = format!("{resp:?}");
    assert!(dbg.contains("Response"));
    assert!(dbg.contains("200"));
}

#[test]
fn tls_info_initially_none() {
    let resp = make_response(200);
    assert!(resp.tls_info().is_none());
}

#[test]
fn links_empty_when_no_link_header() {
    let resp = make_response(200);
    assert!(resp.links().is_empty());
}

#[test]
fn links_parsed_from_header() {
    let inner = http::Response::builder()
        .header("link", "<https://example.com>; rel=\"next\"")
        .body(empty_body())
        .unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let links = resp.links();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].uri(), "https://example.com");
    assert_eq!(links[0].rel(), Some("next"));
}

#[tokio::test]
async fn bytes_returns_body() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("hello"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let bytes = resp.bytes().await.unwrap();
    assert_eq!(&bytes[..], b"hello");
}

#[tokio::test]
async fn text_returns_string() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("world"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let text = resp.text().await.unwrap();
    assert_eq!(text, "world");
}

#[test]
fn from_boxed_constructor() {
    let boxed_body: RequestBodySend = http_body_util::Full::new(bytes::Bytes::new())
        .map_err(|never| match never {})
        .boxed_unsync();
    let inner = http::Response::builder().body(boxed_body).unwrap();
    let resp = Response::from_boxed(inner, "http://example.com".parse().unwrap());
    assert_eq!(resp.status(), StatusCode::OK);
}

#[test]
fn error_for_status_3xx_is_ok() {
    let resp = make_response(301);
    assert!(resp.error_for_status().is_ok());
}

#[test]
fn error_for_status_ref_5xx() {
    let resp = make_response(503);
    assert!(resp.error_for_status_ref().is_err());
}

#[test]
fn is_end_stream_empty_boxed() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    assert!(http_body::Body::is_end_stream(&body));
}

#[test]
fn is_end_stream_non_empty_boxed() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("data"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    assert!(!http_body::Body::is_end_stream(&body));
}

#[test]
fn size_hint_empty_boxed() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let hint = http_body::Body::size_hint(&body);
    assert_eq!(hint.exact(), Some(0));
}

#[test]
fn size_hint_full_boxed() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("hello"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let hint = http_body::Body::size_hint(&body);
    assert_eq!(hint.exact(), Some(5));
}

#[test]
fn error_for_status_1xx_is_ok() {
    let resp = make_response(100);
    assert!(resp.error_for_status().is_ok());
}

#[test]
fn error_for_status_ref_1xx_is_ok() {
    let resp = make_response(100);
    assert!(resp.error_for_status_ref().is_ok());
}

#[test]
fn extensions_mut_can_insert() {
    let mut resp = make_response(200);
    resp.extensions_mut().insert(42u32);
    assert_eq!(resp.extensions().get::<u32>(), Some(&42));
}

#[cfg(feature = "json")]
#[tokio::test]
async fn json_valid() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from(r#"{"key":"value"}"#))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let result: Result<serde_json::Value, _> = resp.json().await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap()["key"], "value");
}

#[cfg(feature = "json")]
#[tokio::test]
async fn json_invalid() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("not json"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let result: Result<serde_json::Value, _> = resp.json().await;
    assert!(result.is_err());
}

#[cfg(feature = "json")]
#[tokio::test]
async fn problem_details_matching_content_type() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from(
            r#"{"type":"about:blank","title":"Not Found","status":404}"#,
        ))
        .map_err(|never| match never {})
        .boxed_unsync(),
    );
    let inner = http::Response::builder()
        .header("content-type", "application/problem+json")
        .body(body)
        .unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let result = resp.problem_details().await;
    assert!(result.is_some());
    let pd = result.unwrap().unwrap();
    assert_eq!(pd.title.as_deref(), Some("Not Found"));
}

#[cfg(feature = "json")]
#[tokio::test]
async fn problem_details_non_matching_content_type() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("{}"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder()
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    assert!(resp.problem_details().await.is_none());
}

#[cfg(feature = "json")]
#[tokio::test]
async fn problem_details_no_content_type() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("{}"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    assert!(resp.problem_details().await.is_none());
}

#[tokio::test]
async fn text_non_utf8() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from(vec![0xff, 0xfe, 0x41]))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let result = resp.text().await;
    #[cfg(not(feature = "charset"))]
    assert!(result.is_err());
    #[cfg(feature = "charset")]
    assert!(result.is_ok());
}

#[test]
fn cookies_parses_set_cookie_headers() {
    let inner = http::Response::builder()
        .header("set-cookie", "session=abc123; Path=/; HttpOnly")
        .header("set-cookie", "lang=en; Path=/")
        .body(empty_body())
        .unwrap();
    let resp = Response::new(inner, "http://example.com/path".parse().unwrap());
    let cookies = resp.cookies();
    assert_eq!(cookies.len(), 2);
    assert!(
        cookies
            .iter()
            .any(|c| c.name() == "session" && c.value() == "abc123")
    );
    assert!(
        cookies
            .iter()
            .any(|c| c.name() == "lang" && c.value() == "en")
    );
}

#[test]
fn cookies_empty_when_no_set_cookie_header() {
    let resp = make_response(200);
    let cookies = resp.cookies();
    assert!(cookies.is_empty());
}

#[test]
fn cookies_skips_malformed_set_cookie() {
    let inner = http::Response::builder()
        .header("set-cookie", "=no_name")
        .header("set-cookie", "good=value")
        .body(empty_body())
        .unwrap();
    let resp = Response::new(inner, "http://example.com/".parse().unwrap());
    let cookies = resp.cookies();
    assert_eq!(cookies.len(), 1);
    assert_eq!(cookies[0].name(), "good");
}

#[test]
fn tls_info_initially_none_and_remote_addr_none() {
    let resp = make_response(200);
    assert!(resp.tls_info().is_none());
    assert!(resp.remote_addr().is_none());
}

#[allow(deprecated)]
#[test]
fn timings_initially_none() {
    let resp = make_response(200);
    assert!(resp.timings().is_none());
}

#[cfg(feature = "charset")]
#[tokio::test]
async fn text_with_charset_latin1() {
    // Latin-1 encoded byte 0xe9 is 'e' with acute accent
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from(vec![0x63, 0x61, 0x66, 0xe9]))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder()
        .header("content-type", "text/plain; charset=iso-8859-1")
        .body(body)
        .unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let text = resp.text().await.unwrap();
    assert_eq!(text, "caf\u{e9}");
}

#[cfg(feature = "charset")]
#[tokio::test]
async fn text_with_charset_defaults_to_utf8() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("hello utf8"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder()
        .header("content-type", "text/plain")
        .body(body)
        .unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello utf8");
}

#[test]
fn into_boxed_from_incoming_variant() {
    // Test the Incoming variant's into_boxed() path
    // We can't easily create a real Incoming, so test via from_boxed path
    let boxed: RequestBodySend = http_body_util::Full::new(bytes::Bytes::from("test"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let body = ResponseBodySend::from_boxed(boxed);
    // Verify the body has data
    assert!(!http_body::Body::is_end_stream(&body));
    let hint = http_body::Body::size_hint(&body);
    assert_eq!(hint.exact(), Some(4));
}

#[test]
fn apply_middleware_modifies_response_headers() {
    use std::sync::Arc;
    let mut stack = crate::middleware::MiddlewareStack::new();
    stack.push(Arc::new(
        |_req: &mut http::Request<RequestBodySend>, _uri: &Uri| {},
    ));
    // Add a middleware that modifies responses
    struct HeaderAdder;
    impl crate::middleware::Middleware for HeaderAdder {
        fn on_response(&self, resp: &mut http::Response<RequestBodySend>, _uri: &Uri) {
            resp.headers_mut()
                .insert("x-modified", http::header::HeaderValue::from_static("yes"));
        }
    }
    let mut stack = crate::middleware::MiddlewareStack::new();
    stack.push(Arc::new(HeaderAdder));

    let uri: Uri = "http://example.com".parse().unwrap();
    let mut resp = make_response(200);
    resp.apply_middleware(&stack, &uri);
    assert_eq!(resp.headers().get("x-modified").unwrap(), "yes");
}

#[test]
fn decompress_passthrough_no_encoding() {
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("raw"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let accept = crate::decompress::AcceptEncoding::default();
    let resp = resp.decompress(&accept);
    assert_eq!(resp.status(), StatusCode::OK);
}

#[cfg(feature = "tokio")]
#[test]
fn apply_read_timeout_wraps_body() {
    use crate::runtime::tokio_rt::TokioRuntime;
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("timeout"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let resp = resp.apply_read_timeout::<TokioRuntime>(std::time::Duration::from_secs(5));
    assert_eq!(resp.status(), StatusCode::OK);
}

#[cfg(feature = "tokio")]
#[test]
fn apply_bandwidth_limit_wraps_body() {
    use crate::runtime::tokio_rt::TokioRuntime;
    let body = ResponseBodySend::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("limited"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let inner = http::Response::builder().body(body).unwrap();
    let resp = Response::new(inner, "http://example.com".parse().unwrap());
    let limiter = crate::bandwidth::BandwidthLimiter::new(1024);
    let resp = resp.apply_bandwidth_limit::<TokioRuntime>(limiter);
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Tests for Response<ResponseBodyLocal> ────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod local_tests {
    use super::*;
    use http_body_util::BodyExt;

    fn local_body(data: &[u8]) -> crate::body::ResponseBodyLocal {
        Box::pin(
            http_body_util::Full::new(bytes::Bytes::from(data.to_vec()))
                .map_err(|never| match never {}),
        )
    }

    fn empty_local_body() -> crate::body::ResponseBodyLocal {
        Box::pin(http_body_util::Full::new(bytes::Bytes::new()).map_err(|never| match never {}))
    }

    fn make_local_response(status: u16) -> Response<crate::body::ResponseBodyLocal> {
        let inner = http::Response::builder()
            .status(status)
            .body(empty_local_body())
            .unwrap();
        Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        }
    }

    #[test]
    fn status_returns_correct_code() {
        let resp = make_local_response(200);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn url_returns_request_uri() {
        let resp = make_local_response(200);
        assert_eq!(resp.url().to_string(), "http://example.com/");
    }

    #[test]
    fn error_for_status_ok_on_2xx() {
        let resp = make_local_response(200);
        assert!(resp.error_for_status().is_ok());
    }

    #[test]
    fn error_for_status_err_on_4xx() {
        let resp = make_local_response(404);
        let err = resp.error_for_status().unwrap_err();
        match err {
            Error::Status(s) => assert_eq!(s, StatusCode::NOT_FOUND),
            _ => panic!("expected Error::Status"),
        }
    }

    #[test]
    fn error_for_status_err_on_5xx() {
        let resp = make_local_response(500);
        assert!(resp.error_for_status().is_err());
    }

    #[test]
    fn error_for_status_ref_ok_on_2xx() {
        let resp = make_local_response(200);
        assert!(resp.error_for_status_ref().is_ok());
    }

    #[test]
    fn error_for_status_ref_err_on_4xx() {
        let resp = make_local_response(403);
        assert!(resp.error_for_status_ref().is_err());
    }

    #[test]
    fn content_length_present() {
        let inner = http::Response::builder()
            .header("Content-Length", "42")
            .body(empty_local_body())
            .unwrap();
        let resp = Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        };
        assert_eq!(resp.content_length(), Some(42));
    }

    #[test]
    fn content_length_missing() {
        let resp = make_local_response(200);
        assert_eq!(resp.content_length(), None);
    }

    #[test]
    fn into_local_conversion() {
        let send_body = ResponseBodySend::from_boxed(
            http_body_util::Full::new(bytes::Bytes::from("test"))
                .map_err(|never| match never {})
                .boxed_unsync(),
        );
        let inner = http::Response::builder()
            .status(201)
            .body(send_body)
            .unwrap();
        let send_resp = Response::new(inner, "http://example.com".parse().unwrap());
        let local_resp = send_resp.into_local();
        assert_eq!(local_resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn bytes_returns_body() {
        let inner = http::Response::builder()
            .body(local_body(b"hello"))
            .unwrap();
        let resp = Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        };
        let bytes = resp.bytes().await.unwrap();
        assert_eq!(&bytes[..], b"hello");
    }

    #[tokio::test]
    async fn text_returns_string() {
        let inner = http::Response::builder()
            .body(local_body(b"world"))
            .unwrap();
        let resp = Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        };
        let text = resp.text().await.unwrap();
        assert_eq!(text, "world");
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn json_valid() {
        let inner = http::Response::builder()
            .body(local_body(br#"{"key":"value"}"#))
            .unwrap();
        let resp = Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        };
        let result: Result<serde_json::Value, _> = resp.json().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["key"], "value");
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn json_invalid() {
        let inner = http::Response::builder()
            .body(local_body(b"not json"))
            .unwrap();
        let resp = Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        };
        let result: Result<serde_json::Value, _> = resp.json().await;
        assert!(result.is_err());
    }

    #[test]
    fn debug_format() {
        let resp = make_local_response(200);
        let dbg = format!("{resp:?}");
        assert!(dbg.contains("Response"));
        assert!(dbg.contains("200"));
    }

    #[tokio::test]
    async fn bytes_local_fires_transfer_complete_observer() {
        use crate::observer::{ConnectionEvent, RequestEvent, RequestObserver, RequestPhase};
        use std::sync::{Arc, Mutex};

        struct RecordingObserver {
            events: Arc<Mutex<Vec<RequestPhase>>>,
        }
        impl RequestObserver for RecordingObserver {
            fn on_event(&self, event: &RequestEvent) {
                self.events.lock().unwrap().push(event.phase.clone());
            }
            fn on_connection_event(&self, _event: &ConnectionEvent) {}
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let inner = http::Response::builder()
            .body(local_body(b"observed"))
            .unwrap();
        let mut resp = Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        };
        resp.set_observer_ctx(super::BodyObserverCtx {
            observer: Arc::new(RecordingObserver {
                events: events.clone(),
            }),
            method: http::Method::GET,
            uri: "http://example.com/".parse().unwrap(),
            response_started: crate::clock::Instant::now(),
        });

        let bytes = resp.bytes().await.unwrap();
        assert_eq!(&bytes[..], b"observed");

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(matches!(captured[0], RequestPhase::TransferComplete { .. }));
    }

    #[tokio::test]
    async fn bytes_local_fires_transfer_aborted_on_error() {
        use crate::observer::{ConnectionEvent, RequestEvent, RequestObserver, RequestPhase};
        use http_body::Body;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};
        use std::task::{Context, Poll};

        struct ErrorBody;
        impl Body for ErrorBody {
            type Data = bytes::Bytes;
            type Error = Error;
            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                Poll::Ready(Some(Err(Error::Other("local error".into()))))
            }
        }

        struct RecordingObserver {
            events: Arc<Mutex<Vec<RequestPhase>>>,
        }
        impl RequestObserver for RecordingObserver {
            fn on_event(&self, event: &RequestEvent) {
                self.events.lock().unwrap().push(event.phase.clone());
            }
            fn on_connection_event(&self, _event: &ConnectionEvent) {}
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let error_body: crate::body::ResponseBodyLocal = Box::pin(ErrorBody);
        let inner = http::Response::builder().body(error_body).unwrap();
        let mut resp = Response {
            inner,
            url: "http://example.com".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        };
        resp.set_observer_ctx(super::BodyObserverCtx {
            observer: Arc::new(RecordingObserver {
                events: events.clone(),
            }),
            method: http::Method::POST,
            uri: "http://example.com/upload".parse().unwrap(),
            response_started: crate::clock::Instant::now(),
        });

        let result = resp.bytes().await;
        assert!(result.is_err());

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(matches!(captured[0], RequestPhase::TransferAborted { .. }));
    }
}
