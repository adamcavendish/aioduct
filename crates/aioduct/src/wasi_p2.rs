//! WASI Preview 2 HTTP client using `wasi:http/outgoing-handler`.
//!
//! This module provides a synchronous HTTP client for the `wasm32-wasip2` target.
//! TLS, connection pooling, and DNS resolution are handled transparently by the
//! WASI runtime (e.g., wasmtime).

use bytes::Bytes;
use http::header::HeaderValue;
use http::{HeaderMap, Method, StatusCode, Uri};
use std::time::Duration;

use crate::error::{BuilderError, Error};

/// HTTP client for WASI Preview 2 environments.
///
/// Uses `wasi:http/outgoing-handler` to make requests. The WASI runtime handles
/// TLS, connection pooling, and DNS resolution transparently.
#[derive(Debug, Clone)]
pub struct WasiClient {
    default_headers: HeaderMap,
}

/// Builder for configuring a [`WasiClient`].
#[derive(Debug, Clone)]
pub struct WasiClientBuilder {
    default_headers: HeaderMap,
    builder_error: Option<BuilderError>,
}

impl WasiClientBuilder {
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

    /// Add a default header applied to every request.
    pub fn default_header(mut self, name: http::header::HeaderName, value: HeaderValue) -> Self {
        self.default_headers.insert(name, value);
        self
    }

    /// Build the client.
    pub fn build(mut self) -> Result<WasiClient, crate::error::Error> {
        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }
        Ok(WasiClient {
            default_headers: self.default_headers,
        })
    }
}

impl WasiClient {
    /// Create a new client with default settings.
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        Self::builder().build().expect("default build")
    }

    /// Create a builder for configuring the client.
    pub fn builder() -> WasiClientBuilder {
        let mut default_headers = HeaderMap::new();
        let ua = concat!("aioduct/", env!("CARGO_PKG_VERSION"));
        if let Ok(val) = HeaderValue::from_str(ua) {
            default_headers.insert(http::header::USER_AGENT, val);
        }
        WasiClientBuilder {
            default_headers,
            builder_error: None,
        }
    }

    /// Start a GET request.
    pub fn get(&self, uri: &str) -> Result<WasiRequestBuilder<'_>, Error> {
        self.request(Method::GET, uri)
    }

    /// Start a HEAD request.
    pub fn head(&self, uri: &str) -> Result<WasiRequestBuilder<'_>, Error> {
        self.request(Method::HEAD, uri)
    }

    /// Start a POST request.
    pub fn post(&self, uri: &str) -> Result<WasiRequestBuilder<'_>, Error> {
        self.request(Method::POST, uri)
    }

    /// Start a PUT request.
    pub fn put(&self, uri: &str) -> Result<WasiRequestBuilder<'_>, Error> {
        self.request(Method::PUT, uri)
    }

    /// Start a PATCH request.
    pub fn patch(&self, uri: &str) -> Result<WasiRequestBuilder<'_>, Error> {
        self.request(Method::PATCH, uri)
    }

    /// Start a DELETE request.
    pub fn delete(&self, uri: &str) -> Result<WasiRequestBuilder<'_>, Error> {
        self.request(Method::DELETE, uri)
    }

    /// Start a request with a custom method.
    pub fn request(&self, method: Method, uri: &str) -> Result<WasiRequestBuilder<'_>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(WasiRequestBuilder {
            client: WasiClientRef::Borrowed(self),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
            connect_timeout: None,
            read_timeout: None,
            no_decompression: false,
            builder_error: None,
        })
    }
}

impl Default for WasiClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A request being built before sending.
#[derive(Debug)]
pub struct WasiRequestBuilder<'a> {
    client: WasiClientRef<'a>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<Bytes>,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    no_decompression: bool,
    builder_error: Option<BuilderError>,
}

#[derive(Debug)]
enum WasiClientRef<'a> {
    Borrowed(&'a WasiClient),
    Owned(WasiClient),
}

impl std::ops::Deref for WasiClientRef<'_> {
    type Target = WasiClient;
    fn deref(&self) -> &WasiClient {
        match self {
            WasiClientRef::Borrowed(r) => r,
            WasiClientRef::Owned(o) => o,
        }
    }
}

impl<'a> WasiRequestBuilder<'a> {
    pub(crate) fn new_owned(
        client: WasiClient,
        method: Method,
        uri: Uri,
    ) -> WasiRequestBuilder<'static> {
        WasiRequestBuilder {
            client: WasiClientRef::Owned(client),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
            connect_timeout: None,
            read_timeout: None,
            no_decompression: false,
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

    /// Set multiple headers at once.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    /// Set the request body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Set a JSON request body.
    #[cfg(feature = "json")]
    pub fn json(mut self, value: &impl serde::Serialize) -> Result<Self, Error> {
        let data =
            serde_json::to_vec(value).map_err(|e| Error::Other(format!("json: {e}").into()))?;
        self.headers
            .entry(http::header::CONTENT_TYPE)
            .or_insert_with(|| HeaderValue::from_static("application/json"));
        self.body = Some(Bytes::from(data));
        Ok(self)
    }

    /// Set a bearer authentication token.
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

    /// Set a request timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Set a timeout for establishing this request's connection.
    pub fn connect_timeout(mut self, duration: Duration) -> Self {
        self.connect_timeout = Some(duration);
        self
    }

    /// Set a timeout for gaps between response body data chunks.
    pub fn read_timeout(mut self, duration: Duration) -> Self {
        self.read_timeout = Some(duration);
        self
    }

    /// Disable automatic response decompression for this request.
    pub fn no_decompression(mut self) -> Self {
        self.no_decompression = true;
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
    /// WASI HTTP negotiates protocol versions through the host runtime.
    pub fn version(mut self, _version: http::Version) -> Self {
        BuilderError::set_once(
            &mut self.builder_error,
            BuilderError::Unsupported("version is not supported by the WASI HTTP runtime".into()),
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

    /// Send the request and return the response.
    pub fn send(mut self) -> Result<WasiResponse, Error> {
        use wasi::http::outgoing_handler;
        use wasi::http::types::{Fields, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};

        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }
        let _raw_response_requested = self.no_decompression;

        let fields = Fields::new();
        for (name, value) in &self.client.default_headers {
            if !self.headers.contains_key(name) {
                let v = value.to_str().map_err(|e| {
                    Error::InvalidHeader(format!(
                        "default header `{}` is not valid WASI HTTP text: {e}",
                        name.as_str()
                    ))
                })?;
                fields.append(name.as_str(), v.as_bytes()).map_err(|e| {
                    Error::InvalidHeader(format!(
                        "failed to append default header `{}`: {e:?}",
                        name.as_str(),
                    ))
                })?;
            }
        }
        for (name, value) in &self.headers {
            let v = value.to_str().map_err(|e| {
                Error::InvalidHeader(format!(
                    "request header `{}` is not valid WASI HTTP text: {e}",
                    name.as_str()
                ))
            })?;
            fields.append(name.as_str(), v.as_bytes()).map_err(|e| {
                Error::InvalidHeader(format!(
                    "failed to append request header `{}`: {e:?}",
                    name.as_str(),
                ))
            })?;
        }

        let request = OutgoingRequest::new(fields);

        let method = match self.method.as_str() {
            "GET" => wasi::http::types::Method::Get,
            "HEAD" => wasi::http::types::Method::Head,
            "POST" => wasi::http::types::Method::Post,
            "PUT" => wasi::http::types::Method::Put,
            "PATCH" => wasi::http::types::Method::Patch,
            "DELETE" => wasi::http::types::Method::Delete,
            other => wasi::http::types::Method::Other(other.to_string()),
        };
        request
            .set_method(&method)
            .map_err(|()| Error::Other("failed to set method".into()))?;

        let scheme = match self.uri.scheme_str() {
            Some("https") => Some(Scheme::Https),
            Some("http") => Some(Scheme::Http),
            Some(other) => Some(Scheme::Other(other.to_string())),
            None => None,
        };
        if let Some(ref s) = scheme {
            request
                .set_scheme(Some(s))
                .map_err(|()| Error::Other("failed to set scheme".into()))?;
        }

        if let Some(authority) = self.uri.authority() {
            request
                .set_authority(Some(authority.as_str()))
                .map_err(|()| Error::Other("failed to set authority".into()))?;
        }

        let path_and_query = self
            .uri
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());
        request
            .set_path_with_query(Some(&path_and_query))
            .map_err(|()| Error::Other("failed to set path".into()))?;

        let outgoing_body = request
            .body()
            .map_err(|_| Error::Other("failed to get outgoing body".into()))?;
        if let Some(body_bytes) = &self.body {
            let stream = outgoing_body
                .write()
                .map_err(|_| Error::Other("failed to get body write stream".into()))?;
            stream
                .blocking_write_and_flush(body_bytes)
                .map_err(|e| Error::Io(std::io::Error::other(format!("{e:?}"))))?;
            drop(stream);
        }
        OutgoingBody::finish(outgoing_body, None)
            .map_err(|_| Error::Other("failed to finish outgoing body".into()))?;

        let options = RequestOptions::new();
        if let Some(t) = self.timeout {
            let nanos = u64::try_from(t.as_nanos()).unwrap_or(u64::MAX);
            options
                .set_connect_timeout(Some(nanos))
                .map_err(|()| Error::Other("failed to set WASI connect timeout".into()))?;
            options
                .set_first_byte_timeout(Some(nanos))
                .map_err(|()| Error::Other("failed to set WASI first-byte timeout".into()))?;
            options
                .set_between_bytes_timeout(Some(nanos))
                .map_err(|()| Error::Other("failed to set WASI between-bytes timeout".into()))?;
        }
        if let Some(t) = self.connect_timeout {
            let nanos = u64::try_from(t.as_nanos()).unwrap_or(u64::MAX);
            options
                .set_connect_timeout(Some(nanos))
                .map_err(|()| Error::Other("failed to set WASI connect timeout".into()))?;
        }
        if let Some(t) = self.read_timeout {
            let nanos = u64::try_from(t.as_nanos()).unwrap_or(u64::MAX);
            options
                .set_between_bytes_timeout(Some(nanos))
                .map_err(|()| Error::Other("failed to set WASI between-bytes timeout".into()))?;
        }

        let future_resp = outgoing_handler::handle(request, Some(options))
            .map_err(|e| Error::Other(format!("outgoing-handler: {e:?}").into()))?;

        let incoming_resp = loop {
            match future_resp.get() {
                Some(result) => {
                    break result
                        .map_err(|()| Error::Other("response already taken".into()))?
                        .map_err(|e| Error::Other(format!("http error: {e:?}").into()))?;
                }
                None => {
                    future_resp.subscribe().block();
                }
            }
        };

        let status = StatusCode::from_u16(incoming_resp.status())
            .map_err(|e| Error::Other(format!("invalid status code: {e}").into()))?;

        let mut headers = HeaderMap::new();
        let resp_headers = incoming_resp.headers();
        for (name, value) in resp_headers.entries() {
            if let (Ok(header_name), Ok(header_value)) = (
                http::header::HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_bytes(&value),
            ) {
                headers.append(header_name, header_value);
            }
        }

        let incoming_body = incoming_resp
            .consume()
            .map_err(|()| Error::Other("failed to consume response body".into()))?;
        let body_stream = incoming_body
            .stream()
            .map_err(|()| Error::Other("failed to get body stream".into()))?;

        Ok(WasiResponse {
            status,
            headers,
            body: WasiBody::Stream {
                incoming_body,
                stream: body_stream,
            },
            url: self.uri,
        })
    }
}

/// HTTP response from a WASI-P2 request.
pub struct WasiResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: WasiBody,
    url: Uri,
}

impl std::fmt::Debug for WasiResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasiResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("url", &self.url)
            .finish()
    }
}

impl Drop for WasiResponse {
    fn drop(&mut self) {
        if let WasiBody::Stream {
            incoming_body,
            stream,
        } = std::mem::replace(&mut self.body, WasiBody::Consumed)
        {
            drop(stream);
            wasi::http::types::IncomingBody::finish(incoming_body);
        }
    }
}

enum WasiBody {
    Stream {
        incoming_body: wasi::http::types::IncomingBody,
        stream: wasi::io::streams::InputStream,
    },
    Consumed,
}

impl WasiResponse {
    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Returns the request URL.
    pub fn url(&self) -> &Uri {
        &self.url
    }

    /// Consume the response and return the body bytes.
    pub fn bytes(mut self) -> Result<Bytes, Error> {
        match std::mem::replace(&mut self.body, WasiBody::Consumed) {
            WasiBody::Stream {
                incoming_body,
                stream,
            } => {
                let mut buf = Vec::new();
                loop {
                    match stream.blocking_read(64 * 1024) {
                        Ok(chunk) => buf.extend_from_slice(&chunk),
                        Err(wasi::io::streams::StreamError::Closed) => break,
                        Err(e) => {
                            return Err(Error::Io(std::io::Error::other(format!(
                                "body read: {e:?}"
                            ))));
                        }
                    }
                }
                drop(stream);
                wasi::http::types::IncomingBody::finish(incoming_body);
                Ok(Bytes::from(buf))
            }
            WasiBody::Consumed => Ok(Bytes::new()),
        }
    }

    /// Consume the response and return the body as UTF-8 text.
    pub fn text(self) -> Result<String, Error> {
        let b = self.bytes()?;
        String::from_utf8(b.to_vec()).map_err(|e| Error::Other(format!("utf-8: {e}").into()))
    }

    /// Deserialize the response body as JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        let b = self.bytes()?;
        serde_json::from_slice(&b).map_err(|e| Error::Other(format!("json: {e}").into()))
    }

    /// Convert the response into a streaming byte reader.
    pub fn into_bytes_stream(mut self) -> WasiBodyStream {
        match std::mem::replace(&mut self.body, WasiBody::Consumed) {
            WasiBody::Stream {
                incoming_body,
                stream,
            } => WasiBodyStream {
                stream: Some(stream),
                incoming_body: Some(incoming_body),
                done: false,
            },
            WasiBody::Consumed => WasiBodyStream {
                stream: None,
                incoming_body: None,
                done: true,
            },
        }
    }

    /// Returns an error if the status code is 4xx or 5xx.
    pub fn error_for_status(self) -> Result<Self, Error> {
        let status = self.status;
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }
}

/// Byte stream over a WASI-P2 response body.
pub struct WasiBodyStream {
    stream: Option<wasi::io::streams::InputStream>,
    incoming_body: Option<wasi::http::types::IncomingBody>,
    done: bool,
}

impl std::fmt::Debug for WasiBodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasiBodyStream")
            .field("done", &self.done)
            .finish()
    }
}

impl WasiBodyStream {
    /// Returns the next chunk of body data, or `None` when complete.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Result<Bytes, Error>> {
        if self.done {
            return None;
        }

        let stream = match &self.stream {
            Some(s) => s,
            None => {
                self.done = true;
                return None;
            }
        };

        match stream.blocking_read(64 * 1024) {
            Ok(chunk) => {
                if chunk.is_empty() {
                    self.done = true;
                    self.finish();
                    None
                } else {
                    Some(Ok(Bytes::from(chunk)))
                }
            }
            Err(wasi::io::streams::StreamError::Closed) => {
                self.done = true;
                self.finish();
                None
            }
            Err(e) => {
                self.done = true;
                self.finish();
                Some(Err(Error::Io(std::io::Error::other(format!(
                    "body read: {e:?}"
                )))))
            }
        }
    }

    fn finish(&mut self) {
        drop(self.stream.take());
        if let Some(body) = self.incoming_body.take() {
            wasi::http::types::IncomingBody::finish(body);
        }
    }
}

impl Drop for WasiBodyStream {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_client_has_user_agent() {
        let client = WasiClient::new();
        assert!(
            client
                .default_headers
                .contains_key(http::header::USER_AGENT)
        );
        let ua = client
            .default_headers
            .get(http::header::USER_AGENT)
            .unwrap();
        assert!(ua.to_str().unwrap().starts_with("aioduct/"));
    }

    #[test]
    fn builder_sets_user_agent() {
        let client = WasiClient::builder()
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
        let err = WasiClient::builder()
            .user_agent("bad\x00agent")
            .build()
            .unwrap_err();
        assert!(matches!(err, Error::InvalidHeader(_)));
    }

    #[test]
    fn method_helpers_build_correctly() {
        let client = WasiClient::new();
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
    fn method_helpers_reject_invalid_urls() {
        let client = WasiClient::new();
        assert!(client.get("not a url").is_err());
        assert!(client.post("htt p://bad url").is_err());
    }

    #[test]
    fn request_builder_sets_header() {
        let client = WasiClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
        assert_eq!(req.headers.get(http::header::ACCEPT).unwrap(), "text/html");
    }

    #[test]
    fn request_builder_sets_body() {
        let client = WasiClient::new();
        let req = client.post("https://example.com").unwrap().body("hello");
        assert_eq!(req.body.as_ref().unwrap(), &Bytes::from("hello"));
    }

    #[test]
    fn request_builder_bearer_auth() {
        let client = WasiClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .bearer_auth("token123");
        assert_eq!(
            req.headers.get(http::header::AUTHORIZATION).unwrap(),
            "Bearer token123"
        );
    }

    #[test]
    fn request_builder_timeout() {
        let client = WasiClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .timeout(Duration::from_secs(30));
        assert_eq!(req.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn request_builder_timeout_controls_are_explicit() {
        let client = WasiClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .connect_timeout(Duration::from_secs(1))
            .read_timeout(Duration::from_secs(2))
            .no_decompression();
        assert_eq!(req.connect_timeout, Some(Duration::from_secs(1)));
        assert_eq!(req.read_timeout, Some(Duration::from_secs(2)));
        assert!(req.no_decompression);
    }

    #[test]
    fn request_builder_version_errors() {
        let client = WasiClient::new();
        let req = client
            .get("https://example.com")
            .unwrap()
            .version(http::Version::HTTP_11);
        assert!(req.builder_error.is_some());
    }

    #[test]
    fn request_builder_basic_auth_with_password() {
        let client = WasiClient::new();
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
        let client = WasiClient::new();
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
        let client = WasiClient::new();
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
        let client = WasiClient::new();
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
        let client = WasiClient::new();
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
    fn default_impl() {
        let client = WasiClient::default();
        assert!(
            client
                .default_headers
                .contains_key(http::header::USER_AGENT)
        );
    }
}
