#![cfg(all(test, feature = "tokio"))]

use crate::client::HttpEngineSend;
use crate::runtime::tokio_rt::TokioRuntime;

fn test_client() -> HttpEngineSend<TokioRuntime, crate::runtime::tokio_rt::TcpConnector> {
    HttpEngineSend::new()
}

#[test]
fn get_valid_url() {
    let client = test_client();
    assert!(client.get("http://example.com").is_ok());
}

#[test]
fn head_valid_url() {
    let client = test_client();
    assert!(client.head("http://example.com").is_ok());
}

#[test]
fn post_valid_url() {
    let client = test_client();
    assert!(client.post("http://example.com").is_ok());
}

#[test]
fn put_valid_url() {
    let client = test_client();
    assert!(client.put("http://example.com").is_ok());
}

#[test]
fn patch_valid_url() {
    let client = test_client();
    assert!(client.patch("http://example.com").is_ok());
}

#[test]
fn delete_valid_url() {
    let client = test_client();
    assert!(client.delete("http://example.com").is_ok());
}

#[test]
fn request_valid_url() {
    let client = test_client();
    assert!(
        client
            .request(http::Method::OPTIONS, "http://example.com")
            .is_ok()
    );
}

#[test]
fn get_invalid_url() {
    let client = test_client();
    assert!(client.get("not a url\n").is_err());
}

#[test]
fn default_timeout_is_none() {
    let client = test_client();
    assert!(client.default_timeout().is_none());
}

#[test]
fn default_retry_is_none() {
    let client = test_client();
    assert!(client.default_retry().is_none());
}

#[test]
fn bandwidth_limiter_is_none() {
    let client = test_client();
    assert!(client.bandwidth_limiter().is_none());
}
