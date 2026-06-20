use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::error::{BuilderError, Error};

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
            builder_error: None,
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
    builder_error: Option<BuilderError>,
}

impl WasmClientBuilder {
    /// Set default headers for all requests.
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers.extend(headers);
        self
    }

    /// Set a default User-Agent header.
    pub fn user_agent(mut self, value: impl AsRef<str>) -> Self {
        match HeaderValue::from_str(value.as_ref()) {
            Ok(val) => {
                self.default_headers.insert(http::header::USER_AGENT, val);
            }
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_header(format!("invalid user-agent header value: {e}")),
            ),
        }
        self
    }

    /// Set a default request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the WASM client.
    pub fn build(mut self) -> Result<WasmClient, crate::error::Error> {
        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }
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
    builder_error: Option<BuilderError>,
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
            builder_error: None,
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
            builder_error: None,
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
        match HeaderValue::from_str(&format!("Bearer {token}")) {
            Ok(val) => {
                self.headers.insert(http::header::AUTHORIZATION, val);
            }
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_header(format!("invalid bearer token header value: {e}")),
            ),
        }
        self
    }

    /// Set a per-request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set a per-request connect timeout.
    ///
    /// Browser Fetch does not expose connection-establishment timing controls.
    pub fn connect_timeout(mut self, _timeout: Duration) -> Self {
        BuilderError::set_once(
            &mut self.builder_error,
            BuilderError::Unsupported(
                "connect_timeout is not supported by the browser fetch runtime".into(),
            ),
        );
        self
    }

    /// Set a per-request read timeout.
    ///
    /// Browser Fetch does not expose response-body read-gap timing controls.
    pub fn read_timeout(mut self, _timeout: Duration) -> Self {
        BuilderError::set_once(
            &mut self.builder_error,
            BuilderError::Unsupported(
                "read_timeout is not supported by the browser fetch runtime".into(),
            ),
        );
        self
    }

    /// Disable automatic response decompression for this request.
    ///
    /// Browser Fetch manages decompression and does not expose raw compressed bytes.
    pub fn no_decompression(mut self) -> Self {
        BuilderError::set_once(
            &mut self.builder_error,
            BuilderError::Unsupported(
                "no_decompression is not supported by the browser fetch runtime".into(),
            ),
        );
        self
    }

    /// Set a Basic Authorization header.
    ///
    /// If the username or password produce an invalid header value, the builder
    /// records an error returned by [`send`](Self::send).
    pub fn basic_auth(mut self, username: &str, password: Option<&str>) -> Self {
        use base64::engine::{Engine, general_purpose::STANDARD};
        let credentials = match password {
            Some(pw) => format!("{username}:{pw}"),
            None => format!("{username}:"),
        };
        let encoded = STANDARD.encode(credentials);
        let Ok(value) = HeaderValue::from_str(&format!("Basic {encoded}")) else {
            BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_header("invalid basic authorization header value"),
            );
            return self;
        };
        self.headers.insert(http::header::AUTHORIZATION, value);
        self
    }

    /// Append URL query parameters from string pairs.
    pub fn query(mut self, params: &[(&str, &str)]) -> Self {
        use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
        use std::fmt::Write;
        const QUERY_ENCODE: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'<')
            .add(b'>')
            .add(b'&')
            .add(b'=')
            .add(b'+')
            .add(b'%');

        let mut uri_str = self.uri.to_string();
        let has_query = self.uri.query().is_some();
        for (i, (key, val)) in params.iter().enumerate() {
            let sep = if i == 0 && !has_query { '?' } else { '&' };
            let key = utf8_percent_encode(key, QUERY_ENCODE);
            let val = utf8_percent_encode(val, QUERY_ENCODE);
            let _ = write!(uri_str, "{sep}{key}={val}");
        }
        match uri_str.parse() {
            Ok(new_uri) => self.uri = new_uri,
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_url(format!("failed to append query parameters: {e}")),
            ),
        }
        self
    }

    /// Force a specific HTTP version.
    ///
    /// Browser Fetch negotiates protocol versions internally.
    pub fn version(mut self, _version: http::Version) -> Self {
        BuilderError::set_once(
            &mut self.builder_error,
            BuilderError::Unsupported(
                "version is not supported by the browser fetch runtime".into(),
            ),
        );
        self
    }

    /// Set a URL-encoded form body from string pairs.
    pub fn form(mut self, params: &[(&str, &str)]) -> Self {
        use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
        use std::fmt::Write;

        const FORM_ENCODE: &AsciiSet = &CONTROLS
            .add(b' ')
            .add(b'"')
            .add(b'#')
            .add(b'<')
            .add(b'>')
            .add(b'&')
            .add(b'=')
            .add(b'+')
            .add(b'%');

        let mut encoded = String::new();
        for (i, (key, val)) in params.iter().enumerate() {
            if i > 0 {
                encoded.push('&');
            }
            let key = utf8_percent_encode(key, FORM_ENCODE);
            let val = utf8_percent_encode(val, FORM_ENCODE);
            let _ = write!(encoded, "{key}={val}");
        }
        let encoded = encoded.replace("%20", "+");
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Some(Bytes::from(encoded));
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
    pub async fn send(mut self) -> Result<WasmResponse, Error> {
        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }

        let url = self.uri.to_string();

        let opts = web_sys::RequestInit::new();
        opts.set_method(self.method.as_str());

        let headers = web_sys::Headers::new()
            .map_err(|e| Error::Other(format!("Headers::new failed: {e:?}").into()))?;

        for (name, value) in &self.client.default_headers {
            if !self.headers.contains_key(name) {
                let v = value.to_str().map_err(|e| {
                    Error::InvalidHeader(format!(
                        "default header `{}` is not valid fetch text: {e}",
                        name.as_str()
                    ))
                })?;
                headers.set(name.as_str(), v).map_err(|e| {
                    Error::InvalidHeader(format!(
                        "failed to set default header `{}`: {e:?}",
                        name.as_str()
                    ))
                })?;
            }
        }
        for (name, value) in &self.headers {
            let v = value.to_str().map_err(|e| {
                Error::InvalidHeader(format!(
                    "request header `{}` is not valid fetch text: {e}",
                    name.as_str()
                ))
            })?;
            headers.set(name.as_str(), v).map_err(|e| {
                Error::InvalidHeader(format!(
                    "failed to set request header `{}`: {e:?}",
                    name.as_str()
                ))
            })?;
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

        Ok(WasmResponse {
            status,
            headers: resp_headers,
            inner: resp,
            url: self.uri,
        })
    }
}

/// An HTTP response from the WASM/Fetch client.
pub struct WasmResponse {
    status: StatusCode,
    headers: HeaderMap,
    inner: web_sys::Response,
    url: Uri,
}

impl std::fmt::Debug for WasmResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("url", &self.url)
            .finish()
    }
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
    pub async fn bytes(self) -> Result<Bytes, Error> {
        let body_promise = self
            .inner
            .array_buffer()
            .map_err(|e| Error::Other(format!("arrayBuffer() failed: {e:?}").into()))?;
        let body_value = JsFuture::from(body_promise)
            .await
            .map_err(|e| Error::Other(format!("body read failed: {e:?}").into()))?;
        let uint8_array = js_sys::Uint8Array::new(&body_value);
        Ok(Bytes::from(uint8_array.to_vec()))
    }

    /// Consume the response and return the body as a string.
    pub async fn text(self) -> Result<String, Error> {
        let b = self.bytes().await?;
        String::from_utf8(b.to_vec())
            .map_err(|e| Error::Other(format!("invalid UTF-8 in response body: {e}").into()))
    }

    /// Deserialize the response body from JSON.
    #[cfg(feature = "json")]
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        let b = self.bytes().await?;
        serde_json::from_slice(&b).map_err(|e| Error::Other(Box::new(e)))
    }

    /// Convert the response into a streaming byte reader.
    pub fn into_bytes_stream(self) -> WasmBodyStream {
        let reader = self.inner.body().and_then(|body| {
            body.get_reader()
                .dyn_into::<web_sys::ReadableStreamDefaultReader>()
                .ok()
        });
        WasmBodyStream {
            reader,
            done: false,
        }
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

/// Async byte stream over a WASM `ReadableStream`.
pub struct WasmBodyStream {
    reader: Option<web_sys::ReadableStreamDefaultReader>,
    done: bool,
}

impl std::fmt::Debug for WasmBodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmBodyStream")
            .field("done", &self.done)
            .finish()
    }
}

impl WasmBodyStream {
    /// Returns the next chunk of body data, or `None` when complete.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        if self.done {
            return None;
        }

        let reader = match &self.reader {
            Some(r) => r,
            None => {
                self.done = true;
                return None;
            }
        };

        let result = match JsFuture::from(reader.read()).await {
            Ok(val) => val,
            Err(e) => {
                self.done = true;
                return Some(Err(Error::Other(
                    format!("stream read failed: {e:?}").into(),
                )));
            }
        };

        let done = js_sys::Reflect::get(&result, &"done".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if done {
            self.done = true;
            return None;
        }

        let value = match js_sys::Reflect::get(&result, &"value".into()) {
            Ok(v) => v,
            Err(e) => {
                self.done = true;
                return Some(Err(Error::Other(
                    format!("stream value read failed: {e:?}").into(),
                )));
            }
        };

        let uint8_array = js_sys::Uint8Array::new(&value);
        Some(Ok(Bytes::from(uint8_array.to_vec())))
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
    fn builder_invalid_user_agent_errors() {
        let err = WasmClient::builder()
            .user_agent("bad\x00agent")
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidHeader(_)));
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
    fn request_builder_platform_managed_controls_error() {
        let client = WasmClient::new();

        assert!(
            client
                .get("https://example.com")
                .unwrap()
                .connect_timeout(Duration::from_secs(1))
                .builder_error
                .is_some()
        );
        assert!(
            client
                .get("https://example.com")
                .unwrap()
                .read_timeout(Duration::from_secs(1))
                .builder_error
                .is_some()
        );
        assert!(
            client
                .get("https://example.com")
                .unwrap()
                .no_decompression()
                .builder_error
                .is_some()
        );
        assert!(
            client
                .get("https://example.com")
                .unwrap()
                .version(http::Version::HTTP_11)
                .builder_error
                .is_some()
        );
    }

    #[test]
    fn request_builder_basic_auth_with_password() {
        let client = WasmClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .basic_auth("user", Some("pass"));
        let auth = req
            .headers
            .get(http::header::AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(auth.starts_with("Basic "));
        use base64::engine::{Engine, general_purpose::STANDARD};
        let decoded = STANDARD.decode(&auth[6..]).unwrap();
        assert_eq!(decoded, b"user:pass");
    }

    #[test]
    fn request_builder_basic_auth_without_password() {
        let client = WasmClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .basic_auth("user", None);
        let auth = req
            .headers
            .get(http::header::AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(auth.starts_with("Basic "));
        use base64::engine::{Engine, general_purpose::STANDARD};
        let decoded = STANDARD.decode(&auth[6..]).unwrap();
        assert_eq!(decoded, b"user:");
    }

    #[test]
    fn request_builder_query_appends_params() {
        let client = WasmClient::new();
        let req = client
            .get("https://example.com/path")
            .unwrap()
            .query(&[("key", "value"), ("foo", "bar")]);
        let uri = req.uri.to_string();
        assert!(uri.contains("?key=value"));
        assert!(uri.contains("&foo=bar"));
    }

    #[test]
    fn request_builder_query_appends_to_existing() {
        let client = WasmClient::new();
        let req = client
            .get("https://example.com/path?a=1")
            .unwrap()
            .query(&[("b", "2")]);
        let uri = req.uri.to_string();
        assert!(uri.contains("a=1"));
        assert!(uri.contains("b=2"));
    }

    #[test]
    fn request_builder_form_sets_content_type_and_body() {
        let client = WasmClient::new();
        let req = client
            .post("https://example.com/path")
            .unwrap()
            .form(&[("name", "hello world"), ("tag", "a&b=c")]);

        assert_eq!(
            req.headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(
            req.body.as_ref().unwrap().as_ref(),
            b"name=hello+world&tag=a%26b%3Dc"
        );
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
}
