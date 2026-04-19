use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::error::{Error, Result};

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
    pub fn new() -> Self {
        let mut default_headers = HeaderMap::new();
        let ua = concat!("aioduct/", env!("CARGO_PKG_VERSION"));
        if let Ok(val) = HeaderValue::from_str(ua) {
            default_headers.insert(http::header::USER_AGENT, val);
        }
        Self {
            default_headers,
            timeout: None,
        }
    }

    /// Create a new builder for configuring the WASM client.
    pub fn builder() -> WasmClientBuilder {
        WasmClientBuilder {
            default_headers: HeaderMap::new(),
            timeout: None,
        }
    }

    /// Start a GET request.
    pub fn get(&self, uri: &str) -> Result<WasmRequestBuilder<'_>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::GET, uri))
    }

    /// Start a HEAD request.
    pub fn head(&self, uri: &str) -> Result<WasmRequestBuilder<'_>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::HEAD, uri))
    }

    /// Start a POST request.
    pub fn post(&self, uri: &str) -> Result<WasmRequestBuilder<'_>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::POST, uri))
    }

    /// Start a PUT request.
    pub fn put(&self, uri: &str) -> Result<WasmRequestBuilder<'_>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::PUT, uri))
    }

    /// Start a PATCH request.
    pub fn patch(&self, uri: &str) -> Result<WasmRequestBuilder<'_>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::PATCH, uri))
    }

    /// Start a DELETE request.
    pub fn delete(&self, uri: &str) -> Result<WasmRequestBuilder<'_>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasmRequestBuilder::new(self, Method::DELETE, uri))
    }

    /// Start a request with a custom method.
    pub fn request(&self, method: Method, uri: &str) -> Result<WasmRequestBuilder<'_>> {
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
            self.default_headers
                .insert(http::header::USER_AGENT, val);
        }
        self
    }

    /// Set a default request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the WASM client.
    pub fn build(self) -> WasmClient {
        let mut client = WasmClient::new();
        client.default_headers.extend(self.default_headers);
        client.timeout = self.timeout;
        client
    }
}

/// A request builder for the WASM client.
pub struct WasmRequestBuilder<'a> {
    client: &'a WasmClient,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<Bytes>,
    timeout: Option<Duration>,
}

impl<'a> WasmRequestBuilder<'a> {
    fn new(client: &'a WasmClient, method: Method, uri: Uri) -> Self {
        Self {
            client,
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
        }
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
    pub fn json<T: serde::Serialize>(mut self, value: &T) -> Result<Self> {
        let json_bytes = serde_json::to_vec(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.body = Some(Bytes::from(json_bytes));
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(self)
    }

    /// Send the request using the browser's Fetch API.
    pub async fn send(self) -> Result<WasmResponse> {
        let url = self.uri.to_string();

        let opts = web_sys::RequestInit::new();
        opts.set_method(self.method.as_str());

        let headers = web_sys::Headers::new()
            .map_err(|e| Error::Other(format!("Headers::new failed: {e:?}").into()))?;

        for (name, value) in &self.client.default_headers {
            if !self.headers.contains_key(name) {
                if let Ok(v) = value.to_str() {
                    let _ = headers.set(name.as_str(), v);
                }
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

        let window: web_sys::Window = js_sys::global()
            .dyn_into()
            .map_err(|_| Error::Other("not in a browser window context".into()))?;

        let resp_promise = window.fetch_with_request(&request);

        let timeout_handle = if let Some(duration) = timeout {
            let controller = abort_controller.clone().unwrap();
            let ms = duration.as_millis() as i32;
            Some(window.set_timeout_with_callback_and_timeout_and_arguments_0(
                &wasm_bindgen::closure::Closure::once_into_js(move || {
                    controller.abort();
                })
                .unchecked_into(),
                ms,
            ).map_err(|e| Error::Other(format!("setTimeout failed: {e:?}").into()))?)
        } else {
            None
        };

        let resp_value = JsFuture::from(resp_promise)
            .await
            .map_err(|e| {
                let msg = js_sys::JSON::stringify(&e)
                    .map(String::from)
                    .unwrap_or_else(|_| format!("{e:?}"));
                if msg.contains("abort") {
                    Error::Timeout
                } else {
                    Error::Other(format!("fetch failed: {msg}").into())
                }
            })?;

        if let Some(handle) = timeout_handle {
            window.clear_timeout_with_handle(handle);
        }

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
                        resp_headers.insert(name, value);
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
    pub fn text(self) -> Result<String> {
        String::from_utf8(self.body.to_vec())
            .map_err(|e| Error::Other(format!("invalid UTF-8 in response body: {e}").into()))
    }

    /// Deserialize the response body from JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| Error::Other(Box::new(e)))
    }

    /// Return an error if the status code indicates failure (4xx or 5xx).
    pub fn error_for_status(self) -> Result<Self> {
        let status = self.status;
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }
}
