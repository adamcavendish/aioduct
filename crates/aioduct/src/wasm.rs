use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::error::Error;

/// A browser-based HTTP client using the Fetch API.
///
/// This client is only available on `wasm32` targets with the `wasm` feature.
/// It delegates all networking to the browser's `fetch()` API, which handles
/// connection pooling, TLS, and HTTP/2 automatically.
#[derive(Clone, Debug)]
pub struct WasmClient {
    default_headers: HeaderMap,
    timeout: Option<Duration>,
}

impl WasmClient {
    /// Create a new WASM client with default settings.
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        Self::builder().build().expect("default build")
    }

    /// Create a new builder for configuring the WASM client.
    pub fn builder() -> WasmClientBuilder {
        WasmClientBuilder {
            default_headers: HeaderMap::new(),
            timeout: None,
        }
    }

    /// Start a GET request.
    pub fn get(&self, uri: &str) -> Result<WasmRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::GET, uri))
    }

    /// Start a HEAD request.
    pub fn head(&self, uri: &str) -> Result<WasmRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::HEAD, uri))
    }

    /// Start a POST request.
    pub fn post(&self, uri: &str) -> Result<WasmRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::POST, uri))
    }

    /// Start a PUT request.
    pub fn put(&self, uri: &str) -> Result<WasmRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::PUT, uri))
    }

    /// Start a PATCH request.
    pub fn patch(&self, uri: &str) -> Result<WasmRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::PATCH, uri))
    }

    /// Start a DELETE request.
    pub fn delete(&self, uri: &str) -> Result<WasmRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::DELETE, uri))
    }

    /// Start a request with a custom method.
    pub fn request(&self, method: Method, uri: &str) -> Result<WasmRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, method, uri))
    }
}

impl Default for WasmClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for configuring a [`WasmClient`].
pub struct WasmClientBuilder {
    default_headers: HeaderMap,
    timeout: Option<Duration>,
}

impl WasmClientBuilder {
    /// Set default headers for all requests.
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers.extend(headers);
        self
    }

    /// Set a default User-Agent header.
    pub fn user_agent(mut self, value: impl AsRef<str>) -> Self {
        if let Ok(val) = HeaderValue::from_str(value.as_ref()) {
            self.default_headers.insert(http::header::USER_AGENT, val);
        }
        self
    }

    /// Set a default request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the WASM client.
    pub fn build(self) -> Result<WasmClient, crate::error::Error> {
        let mut default_headers = self.default_headers;
        if !default_headers.contains_key(http::header::USER_AGENT) {
            let ua = concat!("aioduct/", env!("CARGO_PKG_VERSION"));
            if let Ok(val) = HeaderValue::from_str(ua) {
                default_headers.insert(http::header::USER_AGENT, val);
            }
        }
        Ok(WasmClient {
            default_headers,
            timeout: self.timeout,
        })
    }
}

/// A request builder for the WASM client.
pub struct WasmRequestBuilder<'a> {
    client: WasmClientRef<'a>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<Bytes>,
    timeout: Option<Duration>,
}

enum WasmClientRef<'a> {
    Borrowed(&'a WasmClient),
    Owned(WasmClient),
}

impl std::ops::Deref for WasmClientRef<'_> {
    type Target = WasmClient;
    fn deref(&self) -> &WasmClient {
        match self {
            WasmClientRef::Borrowed(r) => r,
            WasmClientRef::Owned(o) => o,
        }
    }
}

impl<'a> WasmRequestBuilder<'a> {
    fn new(client: &'a WasmClient, method: Method, uri: Uri) -> Self {
        Self {
            client: WasmClientRef::Borrowed(client),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
        }
    }

    pub(crate) fn new_owned(
        client: WasmClient,
        method: Method,
        uri: Uri,
    ) -> WasmRequestBuilder<'static> {
        WasmRequestBuilder {
            client: WasmClientRef::Owned(client),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
        }
    }

    pub(crate) fn uri(&self) -> &Uri {
        &self.uri
    }

    /// Set a request header.
    pub fn header(mut self, name: http::header::HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Set multiple request headers.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    /// Set the request body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Set a Bearer auth token.
    pub fn bearer_auth(mut self, token: &str) -> Self {
        if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
            self.headers.insert(http::header::AUTHORIZATION, val);
        }
        self
    }

    /// Set a per-request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set the body as JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self, Error> {
        let json_bytes = serde_json::to_vec(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.body = Some(Bytes::from(json_bytes));
        self.headers
            .entry(http::header::CONTENT_TYPE)
            .or_insert_with(|| HeaderValue::from_static("application/json"));
        Ok(self)
    }

    /// Send the request using the browser's Fetch API.
    pub async fn send(self) -> Result<WasmResponse, Error> {
        let url = self.uri.to_string();

        let opts = web_sys::RequestInit::new();
        opts.set_method(self.method.as_str());

        let headers = web_sys::Headers::new()
            .map_err(|e| Error::Other(format!("Headers::new failed: {e:?}").into()))?;

        for (name, value) in &self.client.default_headers {
            if !self.headers.contains_key(name)
                && let Ok(v) = value.to_str()
            {
                let _ = headers.set(name.as_str(), v);
            }
        }
        for (name, value) in &self.headers {
            if let Ok(v) = value.to_str() {
                let _ = headers.set(name.as_str(), v);
            }
        }

        opts.set_headers(&headers);

        if let Some(body) = &self.body {
            let uint8_array = js_sys::Uint8Array::from(body.as_ref());
            opts.set_body(&uint8_array);
        }

        let timeout = self.timeout.or(self.client.timeout);
        let abort_controller = if timeout.is_some() {
            let controller = web_sys::AbortController::new()
                .map_err(|e| Error::Other(format!("AbortController::new failed: {e:?}").into()))?;
            opts.set_signal(Some(&controller.signal()));
            Some(controller)
        } else {
            None
        };

        let request = web_sys::Request::new_with_str_and_init(&url, &opts)
            .map_err(|e| Error::Other(format!("Request::new failed: {e:?}").into()))?;

        let global = js_sys::global();
        let (resp_promise, timeout_handle) = if let Ok(window) =
            global.clone().dyn_into::<web_sys::Window>()
        {
            let resp_promise = window.fetch_with_request(&request);
            let timeout_handle = if let (Some(duration), Some(controller)) =
                (timeout, abort_controller.clone())
            {
                let ms = duration.as_millis().min(i32::MAX as u128) as i32;
                Some(
                    window
                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                            &wasm_bindgen::closure::Closure::once_into_js(move || {
                                controller.abort();
                            })
                            .unchecked_into(),
                            ms,
                        )
                        .map_err(|e| Error::Other(format!("setTimeout failed: {e:?}").into()))?,
                )
            } else {
                None
            };
            (resp_promise, timeout_handle)
        } else if let Ok(worker) = global.clone().dyn_into::<web_sys::WorkerGlobalScope>() {
            let resp_promise = worker.fetch_with_request(&request);
            let timeout_handle = if let (Some(duration), Some(controller)) =
                (timeout, abort_controller.clone())
            {
                let ms = duration.as_millis().min(i32::MAX as u128) as i32;
                Some(
                    worker
                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                            &wasm_bindgen::closure::Closure::once_into_js(move || {
                                controller.abort();
                            })
                            .unchecked_into(),
                            ms,
                        )
                        .map_err(|e| Error::Other(format!("setTimeout failed: {e:?}").into()))?,
                )
            } else {
                None
            };
            (resp_promise, timeout_handle)
        } else {
            return Err(Error::Other(
                "unsupported JS global scope (expected Window or WorkerGlobalScope)".into(),
            ));
        };

        let result = JsFuture::from(resp_promise).await.map_err(|e| {
            let msg = js_sys::JSON::stringify(&e)
                .map(String::from)
                .unwrap_or_else(|_| format!("{e:?}"));
            if msg.contains("abort") {
                Error::Timeout
            } else {
                Error::Other(format!("fetch failed: {msg}").into())
            }
        });

        if let Some(handle) = timeout_handle {
            if let Ok(window) = global.clone().dyn_into::<web_sys::Window>() {
                window.clear_timeout_with_handle(handle);
            } else if let Ok(worker) = global.clone().dyn_into::<web_sys::WorkerGlobalScope>() {
                worker.clear_timeout_with_handle(handle);
            }
        }

        let resp_value = result?;

        let resp: web_sys::Response = resp_value
            .dyn_into()
            .map_err(|_| Error::Other("fetch did not return a Response".into()))?;

        let status = StatusCode::from_u16(resp.status())
            .map_err(|e| Error::Other(format!("invalid status code: {e}").into()))?;

        let mut resp_headers = HeaderMap::new();
        let header_entries = resp.headers();
        let iterator = js_sys::try_iter(&header_entries)
            .map_err(|e| Error::Other(format!("headers iteration failed: {e:?}").into()))?;
        if let Some(iter) = iterator {
            for entry in iter {
                let entry =
                    entry.map_err(|e| Error::Other(format!("header entry error: {e:?}").into()))?;
                let pair = js_sys::Array::from(&entry);
                if pair.length() == 2 {
                    let key: String = pair.get(0).as_string().unwrap_or_default();
                    let val: String = pair.get(1).as_string().unwrap_or_default();
                    if let (Ok(name), Ok(value)) = (
                        key.parse::<http::header::HeaderName>(),
                        val.parse::<HeaderValue>(),
                    ) {
                        resp_headers.append(name, value);
                    }
                }
            }
        }

        let body_promise = resp
            .array_buffer()
            .map_err(|e| Error::Other(format!("arrayBuffer() failed: {e:?}").into()))?;
        let body_value = JsFuture::from(body_promise)
            .await
            .map_err(|e| Error::Other(format!("body read failed: {e:?}").into()))?;
        let uint8_array = js_sys::Uint8Array::new(&body_value);
        let body = Bytes::from(uint8_array.to_vec());

        Ok(WasmResponse {
            status,
            headers: resp_headers,
            body,
            url: self.uri,
        })
    }
}

/// An HTTP response from the WASM/Fetch client.
#[derive(Debug)]
pub struct WasmResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    url: Uri,
}

impl WasmResponse {
    /// The HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The request URL.
    pub fn url(&self) -> &Uri {
        &self.url
    }

    /// Consume the response and return the body as bytes.
    pub fn bytes(self) -> Bytes {
        self.body
    }

    /// Consume the response and return the body as a string.
    pub fn text(self) -> Result<String, Error> {
        String::from_utf8(self.body.to_vec())
            .map_err(|e| Error::Other(format!("invalid UTF-8 in response body: {e}").into()))
    }

    /// Deserialize the response body from JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|e| Error::Other(Box::new(e)))
    }

    /// Return an error if the status code indicates failure (4xx or 5xx).
    pub fn error_for_status(self) -> Result<Self, Error> {
        let status = self.status;
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_client_has_user_agent() {
        let client = WasmClient::new();
        assert!(
            client
                .default_headers
                .contains_key(http::header::USER_AGENT)
        );
        let ua = client
            .default_headers
            .get(http::header::USER_AGENT)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ua.starts_with("aioduct/"));
    }

    #[test]
    fn default_creates_same_as_new() {
        let client: WasmClient = Default::default();
        assert!(
            client
                .default_headers
                .contains_key(http::header::USER_AGENT)
        );
    }

    #[test]
    fn builder_sets_timeout() {
        let client = WasmClient::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        assert_eq!(client.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn builder_sets_user_agent() {
        let client = WasmClient::builder()
            .user_agent("custom/1.0")
            .build()
            .unwrap();
        let ua = client
            .default_headers
            .get(http::header::USER_AGENT)
            .unwrap();
        assert_eq!(ua, "custom/1.0");
    }

    #[test]
    fn builder_invalid_user_agent_ignored() {
        let client = WasmClient::builder()
            .user_agent("bad\x00agent")
            .build()
            .unwrap();
        let ua = client
            .default_headers
            .get(http::header::USER_AGENT)
            .unwrap();
        assert!(ua.to_str().unwrap().starts_with("aioduct/"));
    }

    #[test]
    fn builder_default_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom", HeaderValue::from_static("value"));
        let client = WasmClient::builder()
            .default_headers(headers)
            .build()
            .unwrap();
        assert!(client.default_headers.contains_key("x-custom"));
        assert!(
            client
                .default_headers
                .contains_key(http::header::USER_AGENT)
        );
    }

    #[test]
    fn method_helpers_return_ok_for_valid_urls() {
        let client = WasmClient::new();
        assert!(client.get("https://example.com").is_ok());
        assert!(client.head("https://example.com").is_ok());
        assert!(client.post("https://example.com").is_ok());
        assert!(client.put("https://example.com").is_ok());
        assert!(client.patch("https://example.com").is_ok());
        assert!(client.delete("https://example.com").is_ok());
        assert!(
            client
                .request(Method::OPTIONS, "https://example.com")
                .is_ok()
        );
    }

    #[test]
    fn method_helpers_return_err_for_invalid_urls() {
        let client = WasmClient::new();
        assert!(client.get("not a url").is_err());
        assert!(client.post("htt p://bad url").is_err());
    }

    #[test]
    fn request_builder_sets_header() {
        let client = WasmClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
        assert_eq!(req.headers.get(http::header::ACCEPT).unwrap(), "text/html");
    }

    #[test]
    fn request_builder_sets_multiple_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-one", HeaderValue::from_static("1"));
        headers.insert("x-two", HeaderValue::from_static("2"));
        let client = WasmClient::new();
        let req = client.get("https://example.com").unwrap().headers(headers);
        assert_eq!(req.headers.get("x-one").unwrap(), "1");
        assert_eq!(req.headers.get("x-two").unwrap(), "2");
    }

    #[test]
    fn request_builder_sets_body() {
        let client = WasmClient::new();
        let req = client.post("https://example.com").unwrap().body("hello");
        assert_eq!(req.body.as_ref().unwrap().as_ref(), b"hello");
    }

    #[test]
    fn request_builder_bearer_auth() {
        let client = WasmClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .bearer_auth("tok123");
        assert_eq!(
            req.headers.get(http::header::AUTHORIZATION).unwrap(),
            "Bearer tok123"
        );
    }

    #[test]
    fn request_builder_timeout() {
        let client = WasmClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .timeout(Duration::from_secs(5));
        assert_eq!(req.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn response_accessors() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from("hello"),
            url: "https://example.com".parse().unwrap(),
        };
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().is_empty());
        assert_eq!(resp.url().to_string(), "https://example.com/");
    }

    #[test]
    fn response_bytes() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from("hello"),
            url: "https://example.com".parse().unwrap(),
        };
        assert_eq!(resp.bytes(), Bytes::from("hello"));
    }

    #[test]
    fn response_text_valid_utf8() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from("hello world"),
            url: "https://example.com".parse().unwrap(),
        };
        assert_eq!(resp.text().unwrap(), "hello world");
    }

    #[test]
    fn response_text_invalid_utf8() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from_static(&[0xff, 0xfe]),
            url: "https://example.com".parse().unwrap(),
        };
        assert!(resp.text().is_err());
    }

    #[test]
    fn response_error_for_status_ok() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "https://example.com".parse().unwrap(),
        };
        assert!(resp.error_for_status().is_ok());
    }

    #[test]
    fn response_error_for_status_client_error() {
        let resp = WasmResponse {
            status: StatusCode::NOT_FOUND,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "https://example.com".parse().unwrap(),
        };
        let err = resp.error_for_status().unwrap_err();
        assert!(err.is_status());
        assert_eq!(err.status(), Some(StatusCode::NOT_FOUND));
    }

    #[test]
    fn response_error_for_status_server_error() {
        let resp = WasmResponse {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "https://example.com".parse().unwrap(),
        };
        assert!(resp.error_for_status().is_err());
    }

    #[test]
    fn response_error_for_status_redirect_is_ok() {
        let resp = WasmResponse {
            status: StatusCode::FOUND,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "https://example.com".parse().unwrap(),
        };
        assert!(resp.error_for_status().is_ok());
    }

    #[test]
    fn response_debug() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "https://example.com".parse().unwrap(),
        };
        let dbg = format!("{resp:?}");
        assert!(dbg.contains("WasmResponse"));
    }

    #[test]
    fn client_debug_and_clone() {
        let client = WasmClient::new();
        let cloned = client.clone();
        let dbg = format!("{cloned:?}");
        assert!(dbg.contains("WasmClient"));
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_sets_default_content_type() {
        let client = WasmClient::new();
        let req = client
            .post("https://example.com/")
            .unwrap()
            .json(&serde_json::json!({"key": "value"}))
            .unwrap();

        assert_eq!(
            req.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_preserves_existing_content_type() {
        let client = WasmClient::new();
        let req = client
            .post("https://example.com/")
            .unwrap()
            .header(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/vnd.api+json"),
            )
            .json(&serde_json::json!({"key": "value"}))
            .unwrap();

        assert_eq!(
            req.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/vnd.api+json"
        );
    }

    #[cfg(feature = "json")]
    #[test]
    fn response_json() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from(r#"{"key":"value"}"#),
            url: "https://example.com".parse().unwrap(),
        };
        let val: serde_json::Value = resp.json().unwrap();
        assert_eq!(val["key"], "value");
    }

    #[cfg(feature = "json")]
    #[test]
    fn response_json_invalid() {
        let resp = WasmResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from("not json"),
            url: "https://example.com".parse().unwrap(),
        };
        assert!(resp.json::<serde_json::Value>().is_err());
    }
}
