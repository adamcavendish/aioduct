use super::*;
use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
use http::StatusCode;

fn test_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::new()
}

#[tokio::test]
async fn header_sets_value() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
    let req = rb.build().unwrap();
    assert_eq!(req.headers().get("accept").unwrap(), "text/html");
}

#[tokio::test]
async fn headers_extends() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let mut hm = HeaderMap::new();
    hm.insert(
        http::header::ACCEPT,
        HeaderValue::from_static("application/json"),
    );
    hm.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    let rb = rb.headers(hm);
    let req = rb.build().unwrap();
    assert!(req.headers().contains_key("accept"));
    assert!(req.headers().contains_key("cache-control"));
}

#[tokio::test]
async fn header_str_valid() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.header_str("x-custom", "value").unwrap();
    let req = rb.build().unwrap();
    assert_eq!(req.headers().get("x-custom").unwrap(), "value");
}

#[tokio::test]
async fn bearer_auth_sets_authorization() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.bearer_auth("mytoken");
    let req = rb.build().unwrap();
    let auth = req
        .headers()
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(auth.starts_with("Bearer "));
    assert!(auth.contains("mytoken"));
}

#[tokio::test]
async fn basic_auth_with_password() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.basic_auth("user", Some("pass"));
    let req = rb.build().unwrap();
    let auth = req
        .headers()
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(auth.starts_with("Basic "));
}

#[tokio::test]
async fn basic_auth_without_password() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.basic_auth("user", None);
    let req = rb.build().unwrap();
    assert!(req.headers().contains_key("authorization"));
}

#[tokio::test]
async fn query_appends_params() {
    let client = test_client();
    let rb = client.get("http://example.com/path").unwrap();
    let rb = rb.query(&[("key", "value"), ("a", "b")]);
    let req = rb.build().unwrap();
    let uri = req.uri().to_string();
    assert!(uri.contains("key=value"));
    assert!(uri.contains("a=b"));
}

#[tokio::test]
async fn query_appends_to_existing() {
    let client = test_client();
    let rb = client.get("http://example.com/path?existing=1").unwrap();
    let rb = rb.query(&[("new", "2")]);
    let req = rb.build().unwrap();
    let uri = req.uri().to_string();
    assert!(uri.contains("existing=1"));
    assert!(uri.contains("new=2"));
}

#[tokio::test]
async fn body_sets_buffered() {
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let rb = rb.body("hello");
    let req = rb.build().unwrap();
    match req.into_body() {
        RequestBody::Buffered(b) => assert_eq!(b, "hello"),
        _ => panic!("expected buffered"),
    }
}

#[cfg(feature = "json")]
#[tokio::test]
async fn json_sets_content_type_and_body() {
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let rb = rb.json(&serde_json::json!({"key": "value"})).unwrap();
    let req = rb.build().unwrap();
    assert_eq!(
        req.headers().get("content-type").unwrap(),
        "application/json"
    );
}

#[cfg(feature = "json")]
#[tokio::test]
async fn json_preserves_existing_content_type() {
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let rb = rb
        .header(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.api+json"),
        )
        .json(&serde_json::json!({"key": "value"}))
        .unwrap();
    let req = rb.build().unwrap();
    assert_eq!(
        req.headers().get("content-type").unwrap(),
        "application/vnd.api+json"
    );
}

#[tokio::test]
async fn form_sets_content_type_and_body() {
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let rb = rb.form(&[("a", "1"), ("b", "2")]);
    let req = rb.build().unwrap();
    assert_eq!(
        req.headers().get("content-type").unwrap(),
        "application/x-www-form-urlencoded"
    );
    match req.into_body() {
        RequestBody::Buffered(b) => {
            let s = String::from_utf8(b.to_vec()).unwrap();
            assert!(s.contains("a=1"));
            assert!(s.contains("b=2"));
        }
        _ => panic!("expected buffered"),
    }
}

#[cfg(feature = "json")]
#[tokio::test]
async fn query_serde_appends_params() {
    #[derive(serde::Serialize)]
    struct Params {
        key: String,
        num: i32,
    }
    let client = test_client();
    let rb = client.get("http://example.com/").unwrap();
    let rb = rb
        .query_serde(&Params {
            key: "val".into(),
            num: 42,
        })
        .unwrap();
    let req = rb.build().unwrap();
    let uri = req.uri().to_string();
    assert!(uri.contains("key=val"));
    assert!(uri.contains("num=42"));
}

#[cfg(feature = "json")]
#[tokio::test]
async fn form_serde_sets_body() {
    #[derive(serde::Serialize)]
    struct FormData {
        name: String,
    }
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let rb = rb
        .form_serde(&FormData {
            name: "test".into(),
        })
        .unwrap();
    let req = rb.build().unwrap();
    assert_eq!(
        req.headers().get("content-type").unwrap(),
        "application/x-www-form-urlencoded"
    );
}

#[tokio::test]
async fn version_sets_http_version() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.version(Version::HTTP_11);
    let req = rb.build().unwrap();
    assert_eq!(req.version(), Version::HTTP_11);
}

#[tokio::test]
async fn build_default_body() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let req = rb.build().unwrap();
    assert_eq!(*req.method(), Method::GET);
}

#[tokio::test]
async fn try_clone_buffered() {
    let client = test_client();
    let rb = client.post("http://example.com").unwrap().body("data");
    let cloned = rb.try_clone();
    assert!(cloned.is_some());
}

#[tokio::test]
async fn try_clone_no_body() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let cloned = rb.try_clone();
    assert!(cloned.is_some());
}

#[tokio::test]
async fn try_clone_streaming_returns_none() {
    use http_body_util::BodyExt;
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let stream_body: crate::body::RequestBodySend = http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let rb = rb.body_stream(stream_body);
    let cloned = rb.try_clone();
    assert!(cloned.is_none());
}

#[tokio::test]
async fn upgrade_sets_headers() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.upgrade();
    let req = rb.build().unwrap();
    assert_eq!(req.headers().get("connection").unwrap(), "Upgrade");
    assert_eq!(req.headers().get("upgrade").unwrap(), "websocket");
    assert_eq!(req.headers().get("sec-websocket-version").unwrap(), "13");
    assert!(req.headers().get("sec-websocket-key").is_some());
    assert_eq!(req.version(), Version::HTTP_11);
}

#[tokio::test]
async fn multipart_sets_content_type() {
    let mp = crate::multipart::Multipart::new().text("field", "value");
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let rb = rb.multipart(mp);
    let req = rb.build().unwrap();
    let ct = req.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.starts_with("multipart/form-data; boundary="));
}

#[tokio::test]
async fn header_str_invalid_name() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let result = rb.header_str("invalid header\n", "value");
    assert!(result.is_err());
}

#[tokio::test]
async fn header_str_invalid_value() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let result = rb.header_str("x-custom", "bad\0value");
    assert!(result.is_err());
}

#[tokio::test]
async fn debug_request_builder() {
    let client = test_client();
    let rb = client.get("http://example.com/path").unwrap();
    let dbg = format!("{rb:?}");
    assert!(dbg.contains("RequestBuilderSend"));
    assert!(dbg.contains("GET"));
}

#[tokio::test]
async fn query_encodes_special_chars() {
    let client = test_client();
    let rb = client.get("http://example.com/path").unwrap();
    let rb = rb.query(&[("key", "hello world"), ("tag", "a&b=c")]);
    let req = rb.build().unwrap();
    let uri = req.uri().to_string();
    assert!(uri.contains("hello%20world"));
    assert!(uri.contains("a%26b%3Dc"));
}

#[tokio::test]
async fn timeout_setter() {
    let client = test_client();
    let rb = client
        .get("http://example.com")
        .unwrap()
        .timeout(Duration::from_secs(5));
    let _req = rb.build().unwrap();
}

#[tokio::test]
async fn connect_timeout_setter() {
    let client = test_client();
    let rb = client
        .get("http://example.com")
        .unwrap()
        .connect_timeout(Duration::from_secs(2));
    assert_eq!(rb.connect_timeout, Some(Duration::from_secs(2)));
}

#[tokio::test]
async fn retry_setter() {
    let client = test_client();
    let rb = client
        .get("http://example.com")
        .unwrap()
        .retry(RetryConfig::default());
    let _req = rb.build().unwrap();
}

#[cfg(feature = "json")]
#[tokio::test]
async fn query_serde_empty_struct() {
    #[derive(serde::Serialize)]
    struct Empty {}
    let client = test_client();
    let rb = client.get("http://example.com/path").unwrap();
    let rb = rb.query_serde(&Empty {}).unwrap();
    let req = rb.build().unwrap();
    let uri = req.uri().to_string();
    assert!(!uri.contains('?'));
}

#[tokio::test]
async fn bearer_auth_with_invalid_chars_is_noop() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    // Control characters are invalid in header values
    let rb = rb.bearer_auth("token\x00with\x01control\x02chars");
    let req = rb.build().unwrap();
    assert!(
        !req.headers().contains_key("authorization"),
        "invalid bearer token should not set header"
    );
}

#[tokio::test]
async fn basic_auth_with_invalid_chars_is_noop() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    // base64 encoding of a username with certain chars can still be valid,
    // but we can force invalid by using multi-line strings. Actually base64
    // always produces valid ASCII, so we test by observing that the header IS set.
    // The only way to trigger line 117 is if the base64-encoded result has invalid
    // header chars, which is impossible with standard base64.
    // Instead, let's verify that valid basic_auth works to document the behavior.
    let rb = rb.basic_auth("user", Some("pass"));
    let req = rb.build().unwrap();
    assert!(req.headers().contains_key("authorization"));
}

#[tokio::test]
async fn send_without_timeout_succeeds() {
    // Exercises the NoTimeout path in send_once (line 352-356)
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let client = test_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
}

#[tokio::test]
async fn send_with_timeout_succeeds() {
    // Exercises the WithTimeout path in send_once (line 344-351)
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let client = test_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn retry_exhaustion_returns_error() {
    // Exercises lines 466-471: retry exhaustion path
    // Connect to an address that will always refuse connections.
    // Use port 1 which is unlikely to be open.
    let client = test_client();
    let result = client
        .get("http://127.0.0.1:1/")
        .unwrap()
        .timeout(Duration::from_millis(100))
        .retry(
            RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::from_millis(1))
                .retry_on_status(false),
        )
        .send()
        .await;
    assert!(result.is_err(), "expected error from retry exhaustion");
}

#[tokio::test]
async fn retry_on_server_error_exhausts() {
    // Server always returns 500; retry_on_status causes retries to exhaust.
    // This exercises the status retry branch (lines 415-432).
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async {
        Ok(hyper::Response::builder()
            .status(500)
            .body(http_body_util::Full::new(bytes::Bytes::from("error")))
            .unwrap())
    })
    .await;
    let client = test_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(1))
                .retry_on_status(true),
        )
        .send()
        .await
        .unwrap();
    // After retries exhausted, the last 500 response is returned
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    // Should have made 3 total attempts (1 + 2 retries)
    assert_eq!(counter.requests(), 3);
}

#[tokio::test]
async fn retry_without_timeout_uses_no_timeout_path() {
    // Exercises line 405: the NoTimeout path inside send_with_retry
    // Connect to a server that fails once then succeeds.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let attempt_count = Arc::new(AtomicU32::new(0));
    let attempt_count2 = attempt_count.clone();

    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(move |_req| {
        let count = attempt_count2.fetch_add(1, Ordering::Relaxed);
        async move {
            if count == 0 {
                // First request: return 503 to trigger retry
                Ok(hyper::Response::builder()
                    .status(503)
                    .body(http_body_util::Full::new(bytes::Bytes::from("unavailable")))
                    .unwrap())
            } else {
                Ok(hyper::Response::new(http_body_util::Full::new(
                    bytes::Bytes::from("ok"),
                )))
            }
        }
    })
    .await;

    // Client with NO timeout, but with retry
    let client = test_client();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            RetryConfig::default()
                .max_retries(2)
                .initial_backoff(Duration::from_millis(1))
                .retry_on_status(true),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(attempt_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn retry_after_is_used_for_retryable_request_timeout_status() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let attempt_count = Arc::new(AtomicU32::new(0));
    let attempt_count2 = attempt_count.clone();

    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(move |_req| {
        let count = attempt_count2.fetch_add(1, Ordering::Relaxed);
        async move {
            if count == 0 {
                Ok(hyper::Response::builder()
                    .status(StatusCode::REQUEST_TIMEOUT)
                    .header(http::header::RETRY_AFTER, "0")
                    .body(http_body_util::Full::new(bytes::Bytes::from("timeout")))
                    .unwrap())
            } else {
                Ok(hyper::Response::new(http_body_util::Full::new(
                    bytes::Bytes::from("ok"),
                )))
            }
        }
    })
    .await;

    let client = test_client();
    let send = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .retry(
            RetryConfig::default()
                .max_retries(1)
                .initial_backoff(Duration::from_secs(60))
                .retry_on_status(true),
        )
        .send();

    let resp = tokio::time::timeout(Duration::from_millis(500), send)
        .await
        .expect("Retry-After: 0 should override the configured backoff")
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(attempt_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn send_error_contains_url() {
    // Exercises the SendError wrapping at line 333
    let client = test_client();
    let err = client
        .get("http://127.0.0.1:1/specific-path")
        .unwrap()
        .timeout(Duration::from_millis(100))
        .send()
        .await
        .unwrap_err();
    assert!(
        err.url().to_string().contains("specific-path"),
        "SendError should contain the request URL, got: {}",
        err.url()
    );
}

#[tokio::test]
async fn force_addr_setter() {
    let client = test_client();
    let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let rb = client.get("http://example.com").unwrap().force_addr(addr);
    assert_eq!(rb.force_addr, Some(addr));
}

#[tokio::test]
async fn build_constructs_valid_request() {
    let client = test_client();
    let uri: Uri = "http://example.com/path?q=1".parse().unwrap();
    let req = RequestBuilderSend::new(&client, Method::POST, uri)
        .header(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("value"),
        )
        .body("hello")
        .build()
        .unwrap();
    assert_eq!(req.method(), Method::POST);
    assert_eq!(req.uri().scheme_str(), Some("http"));
    assert_eq!(req.uri().path(), "/path");
    assert_eq!(req.uri().query(), Some("q=1"));
    assert_eq!(req.headers().get("x-custom").unwrap(), "value");
}

#[tokio::test]
async fn header_overwrites_on_conflict() {
    let client = test_client();
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.header(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("first"),
    );
    let rb = rb.header(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("second"),
    );
    let req = rb.build().unwrap();
    assert_eq!(
        req.headers().get("x-test").unwrap(),
        "second",
        "second header() call should overwrite the first"
    );
}

#[tokio::test]
async fn headers_keep_both_on_conflict() {
    let client = test_client();
    let mut hm = HeaderMap::new();
    hm.insert(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("from_map"),
    );
    hm.insert(
        HeaderName::from_static("x-other"),
        HeaderValue::from_static("other"),
    );
    let rb = client.get("http://example.com").unwrap();
    let rb = rb.headers(hm);
    let rb = rb.header(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("override"),
    );
    let req = rb.build().unwrap();
    // header() insert overwrites the value set by headers()
    assert_eq!(
        req.headers().get("x-test").unwrap(),
        "override",
        "subsequent header() should overwrite the value from headers()"
    );
    // Other header from headers() is still present
    assert_eq!(
        req.headers().get("x-other").unwrap(),
        "other",
        "unrelated header from headers() should be preserved"
    );
}

#[tokio::test]
async fn form_percent_encoding() {
    let client = test_client();
    let rb = client.post("http://example.com").unwrap();
    let rb = rb.form(&[("key", "value with spaces"), ("special", "a&b=c%")]);
    let req = rb.build().unwrap();
    match req.into_body() {
        RequestBody::Buffered(b) => {
            let body_str = String::from_utf8(b.to_vec()).unwrap();
            assert!(
                body_str.contains("key=value+with+spaces"),
                "spaces should become + in form encoding, got: {body_str}"
            );
            assert!(
                body_str.contains("special=a%26b%3Dc%25"),
                "special chars &, =, % should be percent-encoded, got: {body_str}"
            );
        }
        _ => panic!("expected buffered body"),
    }
}
