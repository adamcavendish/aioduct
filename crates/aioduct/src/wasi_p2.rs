//! WASI Preview 2 HTTP client using `wasi:http/outgoing-handler`.
//!
//! This module provides a synchronous HTTP client for the `wasm32-wasip2` target.
//! TLS, connection pooling, and DNS resolution are handled transparently by the
//! WASI runtime (e.g., wasmtime).

use bytes::Bytes;
use http::header::HeaderValue;
use http::{HeaderMap, Method, StatusCode, Uri};
use std::time::Duration;

use crate::error::Error;

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
}

impl WasiClientBuilder {
    /// Set a default User-Agent header.
    pub fn user_agent(mut self, value: impl AsRef<str>) -> Self {
        if let Ok(val) = HeaderValue::from_str(value.as_ref()) {
            self.default_headers.insert(http::header::USER_AGENT, val);
        }
        self
    }

    /// Add a default header applied to every request.
    pub fn default_header(mut self, name: http::header::HeaderName, value: HeaderValue) -> Self {
        self.default_headers.insert(name, value);
        self
    }

    /// Build the client.
    pub fn build(self) -> Result<WasiClient, crate::error::Error> {
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
        WasiClientBuilder { default_headers }
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
        if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
            self.headers.insert(http::header::AUTHORIZATION, val);
        }
        self
    }

    /// Set a request timeout.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Send the request and return the response.
    pub fn send(self) -> Result<WasiResponse, Error> {
        use wasi::http::outgoing_handler;
        use wasi::http::types::{
            Fields, IncomingBody, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
        };

        let fields = Fields::new();
        for (name, value) in &self.client.default_headers {
            if !self.headers.contains_key(name)
                && let Ok(v) = value.to_str()
            {
                let _ = fields.append(name.as_str(), v.as_bytes());
            }
        }
        for (name, value) in &self.headers {
            if let Ok(v) = value.to_str() {
                let _ = fields.append(name.as_str(), v.as_bytes());
            }
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
            let _ = options.set_connect_timeout(Some(nanos));
            let _ = options.set_first_byte_timeout(Some(nanos));
            let _ = options.set_between_bytes_timeout(Some(nanos));
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

        let mut body_buf = Vec::new();
        loop {
            match body_stream.blocking_read(64 * 1024) {
                Ok(chunk) => body_buf.extend_from_slice(&chunk),
                Err(wasi::io::streams::StreamError::Closed) => break,
                Err(e) => {
                    return Err(Error::Io(std::io::Error::other(format!(
                        "body read: {e:?}"
                    ))));
                }
            }
        }
        drop(body_stream);
        IncomingBody::finish(incoming_body);

        Ok(WasiResponse {
            status,
            headers,
            body: Bytes::from(body_buf),
            url: self.uri,
        })
    }
}

/// HTTP response from a WASI-P2 request.
#[derive(Debug)]
pub struct WasiResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    url: Uri,
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
    pub fn bytes(self) -> Bytes {
        self.body
    }

    /// Consume the response and return the body as UTF-8 text.
    pub fn text(self) -> Result<String, Error> {
        String::from_utf8(self.body.to_vec())
            .map_err(|e| Error::Other(format!("utf-8: {e}").into()))
    }

    /// Deserialize the response body as JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|e| Error::Other(format!("json: {e}").into()))
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
    fn builder_invalid_user_agent_ignored() {
        let client = WasiClient::builder()
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
    fn response_error_for_status_ok() {
        let resp = WasiResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "http://example.com".parse().unwrap(),
        };
        assert!(resp.error_for_status().is_ok());
    }

    #[test]
    fn response_error_for_status_4xx() {
        let resp = WasiResponse {
            status: StatusCode::NOT_FOUND,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "http://example.com".parse().unwrap(),
        };
        assert!(resp.error_for_status().is_err());
    }

    #[test]
    fn response_error_for_status_5xx() {
        let resp = WasiResponse {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headers: HeaderMap::new(),
            body: Bytes::new(),
            url: "http://example.com".parse().unwrap(),
        };
        assert!(resp.error_for_status().is_err());
    }

    #[test]
    fn response_text() {
        let resp = WasiResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from("hello world"),
            url: "http://example.com".parse().unwrap(),
        };
        assert_eq!(resp.text().unwrap(), "hello world");
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
