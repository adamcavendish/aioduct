use std::time::Duration;

use base64::Engine;
use wasm_bindgen::prelude::*;

use aioduct::WasmClient;

#[wasm_bindgen]
pub async fn fetch_url(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;

    Ok(format!("HTTP {status}\n\n{body}"))
}

#[wasm_bindgen]
pub async fn fetch_json(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\n{pretty}"))
}

#[wasm_bindgen]
pub async fn post_json(url: &str, body: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let json_body: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Invalid JSON input: {e}"))?;

    let resp = client
        .post(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .json(&json_body)
        .map_err(|e| format!("JSON serialize error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let resp_json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Response JSON error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&resp_json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\n{pretty}"))
}

#[wasm_bindgen]
pub async fn put_json(url: &str, body: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let json_body: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Invalid JSON input: {e}"))?;

    let resp = client
        .put(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .json(&json_body)
        .map_err(|e| format!("JSON serialize error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let resp_json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Response JSON error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&resp_json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\n{pretty}"))
}

#[wasm_bindgen]
pub async fn delete_request(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .delete(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;

    Ok(format!("HTTP {status}\n\n{body}"))
}

#[wasm_bindgen]
pub async fn patch_json(url: &str, body: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let json_body: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Invalid JSON input: {e}"))?;

    let resp = client
        .patch(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .json(&json_body)
        .map_err(|e| format!("JSON serialize error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let resp_json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Response JSON error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&resp_json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\n{pretty}"))
}

#[wasm_bindgen]
pub async fn fetch_with_headers(url: &str) -> Result<String, String> {
    let client = WasmClient::builder()
        .user_agent("aioduct-demo/1.0")
        .build()
        .map_err(|e| format!("Client build error: {e}"))?;

    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::ACCEPT,
            http::HeaderValue::from_static("application/json"),
        )
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let mut output = format!("HTTP {status}\n\nResponse Headers:\n");
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            output.push_str(&format!("  {name}: {v}\n"));
        }
    }
    output.push_str("\nBody:\n");
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    output.push_str(&serde_json::to_string_pretty(&json).unwrap_or_default());

    Ok(output)
}

#[wasm_bindgen]
pub async fn fetch_with_timeout(url: &str, timeout_ms: u32) -> Result<String, String> {
    let client = WasmClient::builder()
        .timeout(Duration::from_millis(timeout_ms as u64))
        .build()
        .map_err(|e| format!("Client build error: {e}"))?;

    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\n{pretty}"))
}

#[wasm_bindgen]
pub async fn fetch_with_bearer_auth(url: &str, token: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\n{pretty}"))
}

#[wasm_bindgen]
pub async fn fetch_error_handling(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    match resp.error_for_status() {
        Ok(resp) => {
            let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;
            Ok(format!("HTTP {status} (Success)\n\n{body}"))
        }
        Err(e) => Ok(format!(
            "HTTP {status} (Error)\n\naioduct returned: {e}\n\nThis demonstrates automatic error detection for 4xx/5xx responses."
        )),
    }
}

#[wasm_bindgen]
pub async fn fetch_redirect(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let final_url = resp.url().to_string();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!(
        "HTTP {status}\nFinal URL: {final_url}\n(Browser followed the redirect automatically)\n\n{pretty}"
    ))
}

#[wasm_bindgen]
pub async fn fetch_response_headers(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let mut output = format!("HTTP {status}\n\n── Response Headers ──\n");
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            output.push_str(&format!("  {name}: {v}\n"));
        }
    }
    let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;
    output.push_str(&format!("\n── Body ──\n{body}"));

    Ok(output)
}

#[wasm_bindgen]
pub async fn fetch_gzip(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("gzip, deflate, br"),
        )
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let mut output = format!("HTTP {status}\n\n");
    let content_encoding = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(none)")
        .to_string();
    output.push_str(&format!("Content-Encoding: {content_encoding}\n"));
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    if let Some(gzipped) = json.get("gzipped") {
        output.push_str(&format!("Server confirms gzipped: {gzipped}\n"));
    }
    output.push_str(&format!(
        "\n{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    ));

    Ok(output)
}

#[wasm_bindgen]
pub async fn fetch_utf8(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;

    Ok(format!(
        "HTTP {status}\n\nUTF-8 content decoded correctly:\n{body}"
    ))
}

#[wasm_bindgen]
pub async fn fetch_html(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::ACCEPT,
            http::HeaderValue::from_static("text/html"),
        )
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;

    Ok(format!(
        "HTTP {status}\nContent-Type: {content_type}\n\n{body}"
    ))
}

#[wasm_bindgen]
pub async fn fetch_cookies(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::COOKIE,
            http::HeaderValue::from_static("session=abc123; theme=dark"),
        )
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!(
        "HTTP {status}\n\nServer echoed request (including headers):\n{pretty}"
    ))
}

#[wasm_bindgen]
pub async fn post_form_urlencoded(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let form_body = "username=rustacean&language=rust&framework=aioduct";

    let resp = client
        .post(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/x-www-form-urlencoded"),
        )
        .body(form_body)
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\nForm data echoed back:\n{pretty}"))
}

#[wasm_bindgen]
pub async fn fetch_basic_auth(url: &str, user: &str, pass: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let credentials = format!("Basic {}", base64_encode(&format!("{user}:{pass}")));

    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&credentials)
                .map_err(|e| format!("Invalid header value: {e}"))?,
        )
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!("HTTP {status}\n\n{pretty}"))
}

fn base64_encode(input: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}

#[wasm_bindgen]
pub async fn fetch_multiple_sequential(urls: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let url_list: Vec<&str> = urls.split(',').collect();
    let mut output = String::new();

    for (i, url) in url_list.iter().enumerate() {
        let url = url.trim();
        let start = js_sys::Date::now();
        let resp = client
            .get(url)
            .map_err(|e| format!("Request #{} build error: {e}", i + 1))?
            .send()
            .await
            .map_err(|e| format!("Request #{} fetch error: {e}", i + 1))?;

        let elapsed = js_sys::Date::now() - start;
        let status = resp.status();
        let body_len = resp.bytes().len();

        output.push_str(&format!(
            "Request #{}: {} → HTTP {} ({} bytes, {:.0}ms)\n",
            i + 1,
            url,
            status,
            body_len,
            elapsed
        ));
    }
    output.push_str(&format!(
        "\n{} requests completed sequentially.",
        url_list.len()
    ));

    Ok(output)
}

#[wasm_bindgen]
pub async fn fetch_large_response(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let start = js_sys::Date::now();

    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let bytes = resp.bytes();
    let elapsed = js_sys::Date::now() - start;
    let len = bytes.len();

    Ok(format!(
        "HTTP {status}\n\nReceived {len} bytes in {elapsed:.0}ms\nThroughput: {:.1} KB/s\n\nFirst 200 chars:\n{}",
        (len as f64 / elapsed) * 1000.0 / 1024.0,
        String::from_utf8_lossy(&bytes[..200.min(len)])
    ))
}

#[wasm_bindgen]
pub async fn fetch_status_codes(base_url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let codes = [200, 201, 204, 301, 400, 403, 404, 500, 503];
    let mut output = String::from("── Status Code Tour ──\n\n");

    for code in codes {
        let url = format!("{base_url}/{code}");
        let resp = client
            .get(&url)
            .map_err(|e| format!("Request build error: {e}"))?
            .send()
            .await
            .map_err(|e| format!("Fetch error for {code}: {e}"))?;

        let status = resp.status();
        let is_err = status.is_client_error() || status.is_server_error();
        let marker = if is_err { "✗" } else { "✓" };
        output.push_str(&format!(
            "  {marker} {code}: {} ({})\n",
            status.canonical_reason().unwrap_or("Unknown"),
            if is_err {
                "error_for_status() → Err"
            } else {
                "OK"
            }
        ));
    }

    Ok(output)
}

#[wasm_bindgen]
pub async fn fetch_content_negotiation(url: &str, accept: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::ACCEPT,
            http::HeaderValue::from_str(accept)
                .map_err(|e| format!("Invalid Accept header: {e}"))?,
        )
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;

    Ok(format!(
        "HTTP {status}\nRequested: Accept: {accept}\nReceived: Content-Type: {content_type}\n\n{body}"
    ))
}

#[wasm_bindgen]
pub async fn fetch_cache_headers(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .get(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-cache"),
        )
        .header(
            http::header::HeaderName::from_static("x-request-id"),
            http::HeaderValue::from_static("demo-12345"),
        )
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let mut output = format!("HTTP {status}\n\n── Cache-related headers ──\n");
    let cache_headers = [
        "cache-control",
        "etag",
        "last-modified",
        "expires",
        "age",
        "vary",
    ];
    for h in cache_headers {
        if let Some(val) = resp.headers().get(h).and_then(|v| v.to_str().ok()) {
            output.push_str(&format!("  {h}: {val}\n"));
        }
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    output.push_str(&format!(
        "\n── Body ──\n{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    ));

    Ok(output)
}

#[wasm_bindgen]
pub async fn post_binary(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let binary_data: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();

    let resp = client
        .post(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .header(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/octet-stream"),
        )
        .body(binary_data)
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let body = resp.text().map_err(|e| format!("Body read error: {e}"))?;

    Ok(format!(
        "HTTP {status}\n\nSent 256 bytes of binary data (0x00..0xFF)\nServer echoed:\n{body}"
    ))
}

#[wasm_bindgen]
pub async fn head_request(url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let resp = client
        .head(url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let mut output = format!("HTTP {status}\n\n── HEAD response (no body) ──\n\n");
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            output.push_str(&format!("  {name}: {v}\n"));
        }
    }
    let body_len = resp.bytes().len();
    output.push_str(&format!(
        "\nBody length: {body_len} bytes (should be 0 for HEAD)"
    ));

    Ok(output)
}

#[wasm_bindgen]
pub async fn fetch_conditional(url: &str) -> Result<String, String> {
    let client = WasmClient::new();

    // First request: get the ETag
    let resp1 = client
        .get(url)
        .map_err(|e| format!("Request 1 build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Request 1 fetch error: {e}"))?;

    let etag = resp1
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let status1 = resp1.status();
    let _ = resp1.bytes();

    let mut output = format!("── Request 1: Initial fetch ──\nHTTP {status1}\nETag: {etag}\n\n");

    // Second request: conditional with If-None-Match
    if !etag.is_empty() {
        let resp2 = client
            .get(url)
            .map_err(|e| format!("Request 2 build error: {e}"))?
            .header(
                http::header::IF_NONE_MATCH,
                http::HeaderValue::from_str(&etag)
                    .map_err(|e| format!("Invalid ETag header: {e}"))?,
            )
            .send()
            .await
            .map_err(|e| format!("Request 2 fetch error: {e}"))?;

        let status2 = resp2.status();
        output.push_str(&format!(
            "── Request 2: Conditional (If-None-Match: {etag}) ──\nHTTP {status2}\n"
        ));
        if status2.as_u16() == 304 {
            output.push_str(
                "\nServer returned 304 Not Modified — content unchanged, no body transferred!",
            );
        } else {
            output.push_str("\nServer returned fresh content (no 304 — endpoint may not support conditional requests).");
        }
    } else {
        output.push_str("── No ETag received — conditional request not possible ──");
    }

    Ok(output)
}

#[wasm_bindgen]
pub async fn fetch_user_agent_echo(user_agent: &str) -> Result<String, String> {
    let client = WasmClient::builder()
        .user_agent(user_agent)
        .build()
        .map_err(|e| format!("Client build error: {e}"))?;

    let resp = client
        .get("https://httpbin.org/user-agent")
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!(
        "HTTP {status}\n\nCustom User-Agent set to: \"{user_agent}\"\nServer echoes:\n{pretty}"
    ))
}

#[wasm_bindgen]
pub async fn fetch_ip_info() -> Result<String, String> {
    let client = WasmClient::new();

    let resp = client
        .get("https://httpbin.org/ip")
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let ip = json
        .get("origin")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    Ok(format!(
        "HTTP {status}\n\nYour public IP (as seen by httpbin): {ip}\n\nThis request was made from aioduct compiled to WebAssembly,\nrunning in your browser right now."
    ))
}

#[wasm_bindgen]
pub async fn fetch_anything(method: &str, url: &str) -> Result<String, String> {
    let client = WasmClient::new();
    let m: http::Method = method
        .parse()
        .map_err(|_| format!("Invalid HTTP method: {method}"))?;

    let resp = client
        .request(m, url)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Fetch error: {e}"))?;

    let status = resp.status();
    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();

    Ok(format!(
        "HTTP {status}\n\nUsed custom method: {method}\n\n{pretty}"
    ))
}
