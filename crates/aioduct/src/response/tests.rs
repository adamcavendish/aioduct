use super::*;
use http_body_util::BodyExt;

fn empty_body() -> ResponseBoxSendBody {
    ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
fn into_body_returns_boxed() {
    let resp = make_response(200);
    let _body = resp.into_body();
}

#[test]
fn into_bytes_stream_returns_stream() {
    let resp = make_response(200);
    let stream = resp.into_bytes_stream();
    let _dbg = format!("{stream:?}");
}

#[test]
fn into_sse_stream_returns_stream() {
    let resp = make_response(200);
    let _stream = resp.into_sse_stream();
}

#[test]
fn from_boxed_constructor() {
    let boxed_body: RequestBoxBody = http_body_util::Full::new(bytes::Bytes::new())
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
    let body = ResponseBoxSendBody::from_boxed(
        http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    assert!(http_body::Body::is_end_stream(&body));
}

#[test]
fn is_end_stream_non_empty_boxed() {
    let body = ResponseBoxSendBody::from_boxed(
        http_body_util::Full::new(bytes::Bytes::from("data"))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    assert!(!http_body::Body::is_end_stream(&body));
}

#[test]
fn size_hint_empty_boxed() {
    let body = ResponseBoxSendBody::from_boxed(
        http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let hint = http_body::Body::size_hint(&body);
    assert_eq!(hint.exact(), Some(0));
}

#[test]
fn size_hint_full_boxed() {
    let body = ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
    let body = ResponseBoxSendBody::from_boxed(
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
fn into_boxed_roundtrip() {
    let original: RequestBoxBody = http_body_util::Full::new(bytes::Bytes::from("data"))
        .map_err(|never| match never {})
        .boxed_unsync();
    let body = ResponseBoxSendBody::from_boxed(original);
    let _boxed = body.into_boxed();
}
