use super::*;

#[test]
fn test_httpdate_parse() {
    let result = httpdate_parse("Sun, 06 Nov 1994 08:49:37 GMT");
    assert!(result.is_some());
}

#[test]
fn test_httpdate_parse_invalid() {
    assert!(httpdate_parse("not a date").is_none());
    assert!(httpdate_parse("").is_none());
}

#[test]
fn test_httpdate_parse_invalid_month() {
    assert!(httpdate_parse("Sun, 06 Foo 1994 08:49:37 GMT").is_none());
}

#[test]
fn test_httpdate_parse_invalid_time() {
    assert!(httpdate_parse("Sun, 06 Nov 1994 08:49 GMT").is_none());
}

#[test]
fn test_is_cacheable_status() {
    assert!(is_cacheable_status(StatusCode::OK));
    assert!(is_cacheable_status(StatusCode::NOT_FOUND));
    assert!(is_cacheable_status(StatusCode::MOVED_PERMANENTLY));
    assert!(!is_cacheable_status(StatusCode::UNAUTHORIZED));
    assert!(!is_cacheable_status(StatusCode::INTERNAL_SERVER_ERROR));
}

#[test]
fn test_is_cacheable_method() {
    assert!(is_cacheable_method(&Method::GET));
    assert!(is_cacheable_method(&Method::HEAD));
    assert!(!is_cacheable_method(&Method::POST));
    assert!(!is_cacheable_method(&Method::PUT));
    assert!(!is_cacheable_method(&Method::DELETE));
}

#[test]
fn test_is_unsafe_method() {
    assert!(!is_unsafe_method(&Method::GET));
    assert!(!is_unsafe_method(&Method::HEAD));
    assert!(!is_unsafe_method(&Method::OPTIONS));
    assert!(!is_unsafe_method(&Method::TRACE));
    assert!(is_unsafe_method(&Method::POST));
    assert!(is_unsafe_method(&Method::PUT));
    assert!(is_unsafe_method(&Method::DELETE));
    assert!(is_unsafe_method(&Method::PATCH));
}

#[test]
fn test_is_response_cacheable() {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=60".parse().unwrap());
    assert!(is_response_cacheable(StatusCode::OK, &headers));

    let mut headers_ns = HeaderMap::new();
    headers_ns.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    assert!(!is_response_cacheable(StatusCode::OK, &headers_ns));

    let mut headers_private = HeaderMap::new();
    headers_private.insert(CACHE_CONTROL, "private".parse().unwrap());
    assert!(!is_response_cacheable(StatusCode::OK, &headers_private));

    let empty_headers = HeaderMap::new();
    assert!(!is_response_cacheable(StatusCode::OK, &empty_headers));
}

#[test]
fn test_is_response_cacheable_with_etag() {
    let mut headers = HeaderMap::new();
    headers.insert(ETAG, "\"abc\"".parse().unwrap());
    assert!(is_response_cacheable(StatusCode::OK, &headers));
}

#[test]
fn test_is_response_cacheable_with_last_modified() {
    let mut headers = HeaderMap::new();
    headers.insert(
        LAST_MODIFIED,
        "Sun, 06 Nov 1994 08:49:37 GMT".parse().unwrap(),
    );
    assert!(is_response_cacheable(StatusCode::OK, &headers));
}
