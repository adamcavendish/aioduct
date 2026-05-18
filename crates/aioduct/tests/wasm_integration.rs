#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

use aioduct::wasm::WasmClient;

const BASE: &str = "http://127.0.0.1:9877";

#[wasm_bindgen_test]
async fn get_hello() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/hello"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[wasm_bindgen_test]
async fn post_echo_body() {
    let client = WasmClient::new();
    let resp = client
        .post(&format!("{BASE}/echo-body"))
        .unwrap()
        .body("round trip payload")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "round trip payload");
}

#[wasm_bindgen_test]
async fn put_method() {
    let client = WasmClient::new();
    let resp = client
        .put(&format!("{BASE}/echo-method"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "PUT");
}

#[wasm_bindgen_test]
async fn patch_method() {
    let client = WasmClient::new();
    let resp = client
        .patch(&format!("{BASE}/echo-method"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "PATCH");
}

#[wasm_bindgen_test]
async fn delete_method() {
    let client = WasmClient::new();
    let resp = client
        .delete(&format!("{BASE}/echo-method"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "DELETE");
}

#[wasm_bindgen_test]
async fn custom_headers_sent() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/echo-headers"))
        .unwrap()
        .header(
            http::header::HeaderName::from_static("x-custom"),
            http::header::HeaderValue::from_static("test-value"),
        )
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.contains("x-custom: test-value"), "body: {body}");
}

#[wasm_bindgen_test]
async fn bearer_auth_header() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/echo-headers"))
        .unwrap()
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("authorization: Bearer secret-token"),
        "body: {body}"
    );
}

#[wasm_bindgen_test]
async fn status_404() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/status/404"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
    assert!(resp.error_for_status().is_err());
}

#[wasm_bindgen_test]
async fn status_500() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/status/500"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert!(resp.error_for_status().is_err());
}

// Note: User-Agent is a "forbidden" header in the browser Fetch spec,
// so the browser overrides it with its own value. We verify a user-agent
// IS sent (the browser's), not the custom aioduct/ value.
#[wasm_bindgen_test]
async fn default_user_agent() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/echo-headers"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.contains("user-agent:"), "body: {body}");
}

#[wasm_bindgen_test]
#[cfg(feature = "json")]
async fn json_round_trip() {
    let client = WasmClient::new();
    let payload = serde_json::json!({"key": "value", "num": 42});
    let resp = client
        .post(&format!("{BASE}/echo-body"))
        .unwrap()
        .json(&payload)
        .unwrap()
        .send()
        .await
        .unwrap();
    let result: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(result["key"], "value");
    assert_eq!(result["num"], 42);
}

#[wasm_bindgen_test]
async fn head_method() {
    let client = WasmClient::new();
    let resp = client
        .head(&format!("{BASE}/hello"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(
        resp.bytes().await.unwrap().is_empty(),
        "HEAD response should have empty body"
    );
}

#[wasm_bindgen_test]
async fn custom_method_options() {
    let client = WasmClient::new();
    let resp = client
        .request(http::Method::OPTIONS, &format!("{BASE}/echo-method"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
}

#[wasm_bindgen_test]
async fn builder_default_headers() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::HeaderName::from_static("x-default"),
        http::header::HeaderValue::from_static("from-builder"),
    );
    let client = WasmClient::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let resp = client
        .get(&format!("{BASE}/echo-headers"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("x-default: from-builder"),
        "default header from builder should be sent, body: {body}"
    );
}

#[wasm_bindgen_test]
async fn builder_timeout_aborts() {
    let client = WasmClient::builder()
        .timeout(std::time::Duration::from_millis(100))
        .build()
        .unwrap();
    let result = client
        .get(&format!("{BASE}/delay/5000"))
        .unwrap()
        .send()
        .await;
    assert!(result.is_err(), "request should time out");
}

#[wasm_bindgen_test]
async fn per_request_timeout_aborts() {
    let client = WasmClient::new();
    let result = client
        .get(&format!("{BASE}/delay/5000"))
        .unwrap()
        .timeout(std::time::Duration::from_millis(100))
        .send()
        .await;
    assert!(result.is_err(), "request should time out");
}

#[wasm_bindgen_test]
async fn response_bytes() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/bytes/64"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.len(), 64);
    assert!(body.iter().all(|&b| b == 0xAB));
}

#[wasm_bindgen_test]
async fn response_headers_accessible() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/response-headers?x-foo=bar"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("x-foo").map(|v| v.to_str().unwrap()),
        Some("bar"),
        "custom response header should be accessible"
    );
}

#[wasm_bindgen_test]
async fn response_url_matches_request() {
    let client = WasmClient::new();
    let url = format!("{BASE}/echo-url");
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.url().to_string(), format!("{BASE}/echo-url"));
}

#[wasm_bindgen_test]
async fn error_for_status_2xx_ok() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/status/200"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert!(resp.error_for_status().is_ok());
}

#[wasm_bindgen_test]
async fn error_for_status_3xx_ok() {
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/status/301"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert!(resp.error_for_status().is_ok());
}

#[wasm_bindgen_test]
async fn invalid_url_returns_error() {
    let client = WasmClient::new();
    assert!(client.get("not a valid url").is_err());
}

#[wasm_bindgen_test]
async fn multiple_headers_on_request() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::HeaderName::from_static("x-one"),
        http::header::HeaderValue::from_static("1"),
    );
    headers.insert(
        http::header::HeaderName::from_static("x-two"),
        http::header::HeaderValue::from_static("2"),
    );
    let client = WasmClient::new();
    let resp = client
        .get(&format!("{BASE}/echo-headers"))
        .unwrap()
        .headers(headers)
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(body.contains("x-one: 1"), "body: {body}");
    assert!(body.contains("x-two: 2"), "body: {body}");
}

#[wasm_bindgen_test]
async fn post_empty_body() {
    let client = WasmClient::new();
    let resp = client
        .post(&format!("{BASE}/echo-body"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert!(resp.text().await.unwrap().is_empty());
}
