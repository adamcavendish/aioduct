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
    assert_eq!(resp.text().unwrap(), "hello aioduct");
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
    assert_eq!(resp.text().unwrap(), "round trip payload");
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
    assert_eq!(resp.text().unwrap(), "PUT");
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
    assert_eq!(resp.text().unwrap(), "PATCH");
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
    assert_eq!(resp.text().unwrap(), "DELETE");
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
    let body = resp.text().unwrap();
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
    let body = resp.text().unwrap();
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
    let body = resp.text().unwrap();
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
    let result: serde_json::Value = resp.json().unwrap();
    assert_eq!(result["key"], "value");
    assert_eq!(result["num"], 42);
}
