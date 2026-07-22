#![cfg(feature = "tokio")]

use super::super::*;
#[cfg(feature = "rustls")]
use super::builder_tests::install_crypto;
use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_http3_enable_then_disable() {
    install_crypto();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .http3(true)
        .unwrap()
        .http3(false)
        .unwrap()
        .build()
        .unwrap();
    assert!(!client.core.prefer_h3);
}

#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_alt_svc_h3_enable_creates_endpoint() {
    install_crypto();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .alt_svc_h3(true)
        .unwrap()
        .build()
        .unwrap();
    assert!(
        !client.core.prefer_h3,
        "alt_svc_h3 alone should not set prefer_h3"
    );
    assert!(
        client.core.h3_endpoint.is_some(),
        "alt_svc_h3(true) should create an h3 endpoint for opportunistic upgrade"
    );
}

#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_alt_svc_h3_disable_clears_endpoint_without_prefer() {
    install_crypto();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .alt_svc_h3(true)
        .unwrap()
        .alt_svc_h3(false)
        .unwrap()
        .build()
        .unwrap();
    assert!(
        client.core.h3_endpoint.is_none(),
        "alt_svc_h3(false) without prefer_h3 should remove the endpoint"
    );
}

#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_http3_enable_sets_prefer_and_endpoint() {
    install_crypto();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    assert!(client.core.prefer_h3);
    assert!(client.core.h3_endpoint.is_some());
}

#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_http3_0rtt_fails_closed_for_any_setter_order() {
    install_crypto();
    let after_http3 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .http3(true)
        .unwrap()
        .h3_zero_rtt(true)
        .build();
    let after_http3 = match after_http3 {
        Err(error) => error,
        Ok(_) => panic!("enabling HTTP/3 0-RTT must fail closed"),
    };
    assert!(
        matches!(&after_http3, crate::error::Error::Unsupported(message) if message.contains("0-RTT")),
        "unexpected error: {after_http3:?}"
    );

    let before_http3 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .h3_zero_rtt(true)
        .http3(true)
        .unwrap()
        .build();
    let before_http3 = match before_http3 {
        Err(error) => error,
        Ok(_) => panic!("enabling HTTP/3 0-RTT must fail closed"),
    };
    assert!(
        matches!(&before_http3, crate::error::Error::Unsupported(message) if message.contains("0-RTT")),
        "unexpected error: {before_http3:?}"
    );
}

#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_alt_svc_h3_disable_keeps_endpoint_when_prefer_h3() {
    install_crypto();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .http3(true)
        .unwrap()
        .alt_svc_h3(false)
        .unwrap()
        .build()
        .unwrap();
    assert!(
        client.core.prefer_h3,
        "prefer_h3 from http3(true) should persist"
    );
    assert!(
        client.core.h3_endpoint.is_some(),
        "alt_svc_h3(false) should keep endpoint when prefer_h3 is set"
    );
}

#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_http3_without_tls_returns_error() {
    install_crypto();
    // http3(true) without calling .tls() first should fail
    let result = HttpEngineSend::<TokioRuntime, TcpConnector>::builder().http3(true);
    assert!(result.is_err(), "http3(true) without TLS should return Err");
    let err = result.unwrap_err();
    match err {
        crate::error::Error::Other(msg) => {
            let msg_str = msg.to_string();
            assert!(
                msg_str.contains("TLS"),
                "error message should mention TLS, got: {msg_str}"
            );
        }
        other => panic!("expected Error::Other mentioning TLS, got {other:?}"),
    }
}

#[cfg(all(feature = "http3", feature = "rustls", feature = "tokio"))]
#[tokio::test]
async fn builder_h3_zero_rtt_can_be_disabled_before_build() {
    install_crypto();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(crate::tls::RustlsConnector::with_webpki_roots())
        .http3(true)
        .unwrap()
        .h3_zero_rtt(true)
        .h3_zero_rtt(false)
        .build()
        .unwrap();
    assert!(client.core.h3_endpoint.is_some());
}

// ── process_redirect tests ──────────────────────────────────────────

fn make_redirect_response(status: StatusCode, location: &str) -> crate::response::Response {
    use http_body_util::BodyExt;
    let http_resp = http::Response::builder()
        .status(status)
        .header(http::header::LOCATION, location)
        .body(
            http_body_util::Full::new(bytes::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    crate::response::Response::from_boxed(http_resp, "http://origin.com/".parse().unwrap())
}

fn make_test_core() -> HttpEngineCore<crate::body::RequestBodySend> {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();
    client.core
}

#[test]
fn process_redirect_301_changes_method_to_get() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::MOVED_PERMANENTLY, "http://origin.com/new");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core
        .process_redirect(&resp, &uri, Method::POST, None, &mut headers, None)
        .unwrap();
    let (next_uri, next_method, next_body, _frag) = result.unwrap();
    assert_eq!(next_uri.path(), "/new");
    assert_eq!(next_method, Method::GET);
    assert!(next_body.is_none());
}

#[test]
fn process_redirect_302_changes_method_to_get() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/found");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core
        .process_redirect(&resp, &uri, Method::PUT, None, &mut headers, None)
        .unwrap();
    let (next_uri, next_method, _, _frag) = result.unwrap();
    assert_eq!(next_uri.path(), "/found");
    assert_eq!(next_method, Method::GET);
}

#[test]
fn process_redirect_303_changes_method_to_get() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::SEE_OTHER, "http://origin.com/see");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core
        .process_redirect(&resp, &uri, Method::POST, None, &mut headers, None)
        .unwrap();
    let (_, next_method, _, _frag) = result.unwrap();
    assert_eq!(next_method, Method::GET);
}

#[test]
fn process_redirect_307_preserves_method_and_body() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/temp");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();
    let body = crate::body::RequestBody::Buffered(bytes::Bytes::from("hello"));

    let result = core
        .process_redirect(&resp, &uri, Method::POST, Some(body), &mut headers, None)
        .unwrap();
    let (next_uri, next_method, next_body, _frag) = result.unwrap();
    assert_eq!(next_uri.path(), "/temp");
    assert_eq!(next_method, Method::POST);
    assert!(next_body.is_some(), "body should be replayed on 307");
}

#[test]
fn process_redirect_308_preserves_method_and_body() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::PERMANENT_REDIRECT, "http://origin.com/perm");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();
    let body = crate::body::RequestBody::Buffered(bytes::Bytes::from("data"));

    let result = core
        .process_redirect(&resp, &uri, Method::PUT, Some(body), &mut headers, None)
        .unwrap();
    let (_, next_method, next_body, _frag) = result.unwrap();
    assert_eq!(next_method, Method::PUT);
    assert!(next_body.is_some());
}

#[test]
fn process_redirect_307_get_without_body_succeeds() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/next");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    // GET without body on 307 should succeed (no body needed)
    let result = core
        .process_redirect(&resp, &uri, Method::GET, None, &mut headers, None)
        .unwrap();
    let (_, next_method, next_body, _frag) = result.unwrap();
    assert_eq!(next_method, Method::GET);
    assert!(next_body.is_none());
}

#[test]
fn process_redirect_307_head_without_body_succeeds() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/next");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core
        .process_redirect(&resp, &uri, Method::HEAD, None, &mut headers, None)
        .unwrap();
    let (_, next_method, _, _frag) = result.unwrap();
    assert_eq!(next_method, Method::HEAD);
}

#[test]
fn process_redirect_307_post_without_body_fails() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::TEMPORARY_REDIRECT, "http://origin.com/next");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    // POST without body on 307 - cannot replay streaming body
    let result = core.process_redirect(&resp, &uri, Method::POST, None, &mut headers, None);
    let err = result.unwrap_err();
    match err {
        crate::error::Error::Redirect(msg) => {
            assert!(
                msg.contains("cannot replay"),
                "expected 'cannot replay' error, got: {msg}"
            );
        }
        other => panic!("expected Redirect error, got {other:?}"),
    }
}

#[test]
fn process_redirect_unexpected_status_returns_error() {
    use http_body_util::BodyExt;
    let core = make_test_core();
    // Use a status that is not a redirect (e.g., 299 or 305)
    let http_resp = http::Response::builder()
        .status(StatusCode::from_u16(305).unwrap())
        .header(http::header::LOCATION, "http://origin.com/proxy")
        .body(
            http_body_util::Full::new(bytes::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let resp =
        crate::response::Response::from_boxed(http_resp, "http://origin.com/".parse().unwrap());
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core.process_redirect(&resp, &uri, Method::GET, None, &mut headers, None);
    let err = result.unwrap_err();
    match err {
        crate::error::Error::Redirect(msg) => {
            assert!(
                msg.contains("unexpected redirect status"),
                "expected 'unexpected redirect status', got: {msg}"
            );
        }
        other => panic!("expected Redirect error, got {other:?}"),
    }
}

#[test]
fn process_redirect_cross_origin_strips_sensitive_headers() {
    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .sensitive_header(http::header::HeaderName::from_static("x-api-key"))
        .build()
        .unwrap()
        .core;
    let resp = make_redirect_response(StatusCode::FOUND, "http://other.com/new");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(http::header::AUTHORIZATION, "Bearer token".parse().unwrap());
    headers.insert(http::header::COOKIE, "session=abc".parse().unwrap());
    headers.insert(
        http::header::HeaderName::from_static("x-api-key"),
        "secret".parse().unwrap(),
    );

    let result = core
        .process_redirect(&resp, &uri, Method::GET, None, &mut headers, None)
        .unwrap();
    assert!(result.is_some());
    // Sensitive headers should be stripped on cross-origin redirect
    assert!(
        headers.get(http::header::AUTHORIZATION).is_none(),
        "Authorization should be stripped"
    );
    assert!(
        headers.get(http::header::COOKIE).is_none(),
        "Cookie should be stripped"
    );
    assert!(
        headers
            .get(http::header::HeaderName::from_static("x-api-key"))
            .is_none(),
        "custom sensitive header should be stripped"
    );
}

#[test]
fn process_redirect_same_origin_preserves_sensitive_headers() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(http::header::AUTHORIZATION, "Bearer token".parse().unwrap());

    let result = core
        .process_redirect(&resp, &uri, Method::GET, None, &mut headers, None)
        .unwrap();
    assert!(result.is_some());
    // Same-origin redirect should keep Authorization
    assert!(
        headers.get(http::header::AUTHORIZATION).is_some(),
        "Authorization should be preserved on same-origin redirect"
    );
}

#[test]
fn process_redirect_sets_host_header() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::FOUND, "http://newhost.com/path");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core
        .process_redirect(&resp, &uri, Method::GET, None, &mut headers, None)
        .unwrap();
    let (next_uri, _, _, _frag) = result.unwrap();
    assert_eq!(next_uri.host(), Some("newhost.com"));
    // Host header should be set to new authority
    assert_eq!(headers.get(http::header::HOST).unwrap(), "newhost.com");
}

#[test]
fn process_redirect_with_referer_enabled() {
    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .referer(true)
        .build()
        .unwrap()
        .core;
    let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let _ = core
        .process_redirect(&resp, &uri, Method::GET, None, &mut headers, None)
        .unwrap();
    // Referer should be set to the current URI
    let referer = headers
        .get(http::header::REFERER)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        referer.contains("origin.com/old"),
        "Referer should contain the previous URI, got: {referer}"
    );
}

#[test]
fn process_redirect_missing_location_returns_error() {
    use http_body_util::BodyExt;
    let core = make_test_core();
    // Response without Location header
    let http_resp = http::Response::builder()
        .status(StatusCode::FOUND)
        .body(
            http_body_util::Full::new(bytes::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let resp =
        crate::response::Response::from_boxed(http_resp, "http://origin.com/".parse().unwrap());
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core.process_redirect(&resp, &uri, Method::GET, None, &mut headers, None);
    let err = result.unwrap_err();
    match err {
        crate::error::Error::Redirect(msg) => {
            assert!(msg.contains("Location"), "got: {msg}");
        }
        other => panic!("expected Redirect error, got {other:?}"),
    }
}

#[test]
fn process_redirect_middleware_notified() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TrackingMiddleware {
        called: Arc<AtomicBool>,
    }
    impl crate::middleware::Middleware for TrackingMiddleware {
        fn on_redirect(&self, _status: StatusCode, _from: &Uri, _to: &Uri) {
            self.called.store(true, Ordering::SeqCst);
        }
    }

    let called = Arc::new(AtomicBool::new(false));
    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .middleware(TrackingMiddleware {
            called: called.clone(),
        })
        .build()
        .unwrap()
        .core;

    let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let _ = core
        .process_redirect(&resp, &uri, Method::GET, None, &mut headers, None)
        .unwrap();
    assert!(
        called.load(Ordering::SeqCst),
        "middleware on_redirect should be called"
    );
}

// ── prepare_request_headers tests ───────────────────────────────────

#[test]
fn prepare_request_headers_applies_cookies() {
    let jar = crate::cookie::CookieJar::new();
    let mut cookie_headers = http::HeaderMap::new();
    cookie_headers.insert(
        http::header::SET_COOKIE,
        "session=abc123; Path=/".parse().unwrap(),
    );
    jar.store_from_response("example.com", "/", &cookie_headers);

    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(jar)
        .build()
        .unwrap()
        .core;

    let uri: Uri = "http://example.com/page".parse().unwrap();
    let mut headers = HeaderMap::new();
    core.prepare_request_headers_tracking(&uri, None, &mut headers);

    let cookie_header = headers.get(http::header::COOKIE).unwrap().to_str().unwrap();
    assert!(
        cookie_header.contains("session=abc123"),
        "expected cookie in header, got: {cookie_header}"
    );
}

#[test]
fn refresh_replay_headers_replaces_only_the_previous_jar_cookie() {
    let jar = crate::cookie::CookieJar::new();
    let mut first_response = http::HeaderMap::new();
    first_response.insert(
        http::header::SET_COOKIE,
        "session=old; Path=/".parse().unwrap(),
    );
    jar.store_from_response("example.com", "/", &first_response);

    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(jar.clone())
        .build()
        .unwrap()
        .core;
    let uri: Uri = "http://example.com/page".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(http::header::COOKIE, "caller=preserved".parse().unwrap());
    let previous = core
        .prepare_request_headers_tracking(&uri, None, &mut headers)
        .unwrap();

    let mut redirected_response = http::HeaderMap::new();
    redirected_response.insert(
        http::header::SET_COOKIE,
        "session=new; Path=/".parse().unwrap(),
    );
    jar.store_from_response("example.com", "/", &redirected_response);

    let current = core
        .refresh_replay_headers(&uri, None, Some(&previous), &mut headers)
        .unwrap();
    assert_eq!(current, "session=new");
    assert_eq!(
        headers.get(http::header::COOKIE).unwrap(),
        "caller=preserved; session=new"
    );
}

#[test]
fn prepare_request_headers_sets_host_when_missing() {
    let core = make_test_core();
    let uri: Uri = "http://example.com:8080/path".parse().unwrap();
    let mut headers = HeaderMap::new();

    core.prepare_request_headers_tracking(&uri, None, &mut headers);

    assert_eq!(headers.get(http::header::HOST).unwrap(), "example.com:8080");
}

#[test]
fn prepare_request_headers_does_not_overwrite_existing_host() {
    let core = make_test_core();
    let uri: Uri = "http://example.com/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(http::header::HOST, "custom-host.com".parse().unwrap());

    core.prepare_request_headers_tracking(&uri, None, &mut headers);

    assert_eq!(headers.get(http::header::HOST).unwrap(), "custom-host.com");
}

#[test]
fn prepare_request_headers_no_cookie_jar_is_noop() {
    let core = make_test_core();
    let uri: Uri = "http://example.com/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(http::header::HOST, "already-set".parse().unwrap());

    core.prepare_request_headers_tracking(&uri, None, &mut headers);

    assert!(headers.get(http::header::COOKIE).is_none());
    assert_eq!(headers.get(http::header::HOST).unwrap(), "already-set");
}

// ── post_execute tests ──────────────────────────────────────────────

#[test]
fn post_execute_done_on_non_redirect() {
    use http_body_util::BodyExt;
    let core = make_test_core();
    let http_resp = http::Response::builder()
        .status(StatusCode::OK)
        .body(
            http_body_util::Full::new(bytes::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let resp =
        crate::response::Response::from_boxed(http_resp, "http://example.com/".parse().unwrap());
    let uri: Uri = "http://example.com/page".parse().unwrap();
    let mut headers = HeaderMap::new();

    let action = core
        .post_execute(&resp, &Method::GET, &uri, &mut headers, None, None)
        .unwrap();
    assert!(matches!(
        action,
        super::request_flow::PostExecuteAction::Done
    ));
}

#[test]
fn post_execute_done_on_not_modified() {
    use http_body_util::BodyExt;
    let http_resp = http::Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .body(
            http_body_util::Full::new(bytes::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let resp =
        crate::response::Response::from_boxed(http_resp, "http://example.com/".parse().unwrap());
    let core = make_test_core();
    let uri: Uri = "http://example.com/page".parse().unwrap();
    let mut headers = HeaderMap::new();

    let action = core
        .post_execute(&resp, &Method::GET, &uri, &mut headers, None, None)
        .unwrap();
    assert!(matches!(
        action,
        super::request_flow::PostExecuteAction::Done
    ));
}

#[test]
fn post_execute_redirect_on_302() {
    let core = make_test_core();
    let resp = make_redirect_response(StatusCode::FOUND, "http://example.com/new");
    let uri: Uri = "http://example.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let action = core
        .post_execute(&resp, &Method::GET, &uri, &mut headers, None, None)
        .unwrap();
    match action {
        super::request_flow::PostExecuteAction::Redirect {
            uri,
            method,
            body,
            fragment: _,
        } => {
            assert_eq!(uri.path(), "/new");
            assert_eq!(method, Method::GET);
            assert!(body.is_none());
        }
        super::request_flow::PostExecuteAction::Done => {
            panic!("expected Redirect action");
        }
    }
}

#[test]
fn post_execute_stores_cookies_and_learns_hsts() {
    let jar = crate::cookie::CookieJar::new();
    let store = crate::hsts::HstsStore::new();

    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cookie_jar(jar.clone())
        .hsts(store.clone())
        .build()
        .unwrap()
        .core;

    use http_body_util::BodyExt;
    let http_resp = http::Response::builder()
        .status(StatusCode::OK)
        .header(http::header::SET_COOKIE, "token=xyz; Path=/")
        .header("strict-transport-security", "max-age=31536000")
        .body(
            http_body_util::Full::new(bytes::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let resp = crate::response::Response::from_boxed(
        http_resp,
        "https://secure.example.com/api".parse().unwrap(),
    );

    let uri: Uri = "https://secure.example.com/api".parse().unwrap();
    let mut headers = HeaderMap::new();

    let action = core
        .post_execute(&resp, &Method::GET, &uri, &mut headers, None, None)
        .unwrap();
    assert!(matches!(
        action,
        super::request_flow::PostExecuteAction::Done
    ));

    assert!(
        store.should_upgrade("secure.example.com"),
        "HSTS should be learned from response"
    );

    let mut cookie_headers = HeaderMap::new();
    jar.apply_to_request("secure.example.com", true, "/", None, &mut cookie_headers);
    let cookie_val = cookie_headers
        .get(http::header::COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        cookie_val.contains("token=xyz"),
        "cookie should be stored from response, got: {cookie_val}"
    );
}

#[test]
fn post_execute_invalidates_cache_on_non_safe_method() {
    let cache = crate::cache::HttpCache::new();

    let status = StatusCode::OK;
    let mut resp_headers = http::HeaderMap::new();
    resp_headers.insert(http::header::CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &"http://example.com/data".parse().unwrap(),
        status,
        &resp_headers,
        &bytes::Bytes::from("cached"),
        &HeaderMap::new(),
    );

    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache.clone())
        .build()
        .unwrap()
        .core;

    use http_body_util::BodyExt;
    let http_resp = http::Response::builder()
        .status(StatusCode::OK)
        .body(
            http_body_util::Full::new(bytes::Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let resp = crate::response::Response::from_boxed(
        http_resp,
        "http://example.com/data".parse().unwrap(),
    );
    let uri: Uri = "http://example.com/data".parse().unwrap();
    let mut headers = HeaderMap::new();

    let _ = core
        .post_execute(&resp, &Method::POST, &uri, &mut headers, None, None)
        .unwrap();

    let lookup_headers = HeaderMap::new();
    let result = cache.lookup(&Method::GET, &uri, &lookup_headers);
    assert!(
        matches!(result, crate::cache::CacheLookup::Miss),
        "cache should be invalidated after POST"
    );
}

#[test]
fn post_execute_redirect_applies_hsts_upgrade() {
    let store = crate::hsts::HstsStore::new();
    let mut sts_headers = http::HeaderMap::new();
    sts_headers.insert(
        http::header::HeaderName::from_static("strict-transport-security"),
        "max-age=31536000".parse().unwrap(),
    );
    store.store_from_response("target.example.com", &sts_headers);

    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .hsts(store)
        .build()
        .unwrap()
        .core;

    let resp = make_redirect_response(StatusCode::FOUND, "http://target.example.com/redirected");
    let uri: Uri = "http://origin.example.com/start".parse().unwrap();
    let mut headers = HeaderMap::new();

    let action = core
        .post_execute(&resp, &Method::GET, &uri, &mut headers, None, None)
        .unwrap();
    match action {
        super::request_flow::PostExecuteAction::Redirect { uri, .. } => {
            assert_eq!(
                uri.scheme_str(),
                Some("https"),
                "redirect target should be HSTS-upgraded to https"
            );
            assert_eq!(uri.host(), Some("target.example.com"));
        }
        _ => panic!("expected Redirect action"),
    }
}

#[test]
fn post_execute_redirect_https_only_rejects_http_target() {
    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .https_only(true)
        .build()
        .unwrap()
        .core;

    let resp = make_redirect_response(StatusCode::FOUND, "http://insecure.com/page");
    let uri: Uri = "https://origin.com/start".parse().unwrap();
    let mut headers = HeaderMap::new();

    let result = core.post_execute(&resp, &Method::GET, &uri, &mut headers, None, None);
    let err = result.unwrap_err();
    assert!(
        matches!(err, crate::error::Error::HttpsOnly(_)),
        "expected HttpsOnly error, got: {err:?}"
    );
}

#[test]
fn post_execute_done_when_redirect_policy_none() {
    let core = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .redirect_policy(crate::redirect::RedirectPolicy::None)
        .build()
        .unwrap()
        .core;

    let resp = make_redirect_response(StatusCode::FOUND, "http://origin.com/new");
    let uri: Uri = "http://origin.com/old".parse().unwrap();
    let mut headers = HeaderMap::new();

    let action = core
        .post_execute(&resp, &Method::GET, &uri, &mut headers, None, None)
        .unwrap();
    assert!(
        matches!(action, super::request_flow::PostExecuteAction::Done),
        "should return Done when redirect policy is None"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn hsts_include_subdomains_end_to_end() {
    use bytes::Bytes;
    use http_body_util::Full;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    install_crypto();

    let sub_requested = Arc::new(AtomicBool::new(false));
    let sub_requested_clone = sub_requested.clone();

    let (addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], move |req| {
            let sub = sub_requested_clone.clone();
            async move {
                let host = req
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if host.starts_with("sub.") {
                    sub.store(true, Ordering::SeqCst);
                }
                let response = http::Response::builder()
                    .status(200)
                    .header(
                        "strict-transport-security",
                        "max-age=3600; includeSubDomains",
                    )
                    .body(Full::new(Bytes::from("ok")))
                    .unwrap();
                Ok::<_, Infallible>(response)
            }
        })
        .await;

    let cert = crate::tls::Certificate::from_der(cert_der.to_vec());
    let store = crate::hsts::HstsStore::new();

    // Use localhost hostname to match the TLS cert from generate_self_signed
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .hsts(store.clone())
        .add_root_certificates(&[cert])
        .danger_accept_invalid_hostnames(true)
        .timeout(std::time::Duration::from_secs(5))
        .resolver({
            move |_host: &str, _port: u16| {
                Box::pin(async move { Ok(addr) })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<Output = std::io::Result<std::net::SocketAddr>>
                                + Send,
                        >,
                    >
            }
        })
        .build()
        .unwrap();

    // First request: HTTPS — server returns HSTS header with includeSubDomains
    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
    assert!(
        store.should_upgrade("localhost"),
        "HSTS should be stored for localhost"
    );
    assert!(
        store.should_upgrade("sub.localhost"),
        "includeSubDomains should cover sub.localhost"
    );

    // Second request: http://sub.localhost should be upgraded to https://
    let resp = client
        .get(&format!("http://sub.localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
    assert!(
        sub_requested.load(Ordering::SeqCst),
        "sub.localhost should have been requested (via HTTPS upgrade)"
    );
}
