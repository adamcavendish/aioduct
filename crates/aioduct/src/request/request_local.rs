use std::fmt::Write as _;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};

use crate::body::{RequestBody, RequestBodySend};
use crate::client::HttpEngineLocal;
use crate::error::Error;
use crate::pool::ProtocolHint;
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal};
use crate::timeout::Timeout;

use super::EngineRef;

/// Builder for configuring and sending an HTTP request on a `!Send` runtime.
#[must_use = "a RequestBuilder does nothing unless you call `.send()` or `.build()`"]
pub struct RequestBuilderLocal<'a, R: RuntimeLocal, C: ConnectorLocal + Clone> {
    client: EngineRef<'a, HttpEngineLocal<R, C>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<RequestBody>,
    version: Option<Version>,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
    no_decompression: bool,
    force_no_timeout: bool,
    force_addr: Option<std::net::SocketAddr>,
    protocol_hint: ProtocolHint,
    /// Original URL fragment from the user-provided URL string.
    /// Preserved across redirects per RFC 7231 Section 7.1.2.
    fragment: Option<String>,
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> std::fmt::Debug for RequestBuilderLocal<'_, R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestBuilderLocal")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .finish()
    }
}

impl<'a, R: RuntimeLocal, C: ConnectorLocal + Clone> RequestBuilderLocal<'a, R, C> {
    pub(crate) fn new(
        client: &'a HttpEngineLocal<R, C>,
        method: Method,
        uri: Uri,
        fragment: Option<String>,
    ) -> Self {
        Self {
            client: EngineRef::Borrowed(client),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            version: None,
            timeout: None,
            connect_timeout: None,
            read_timeout: None,
            write_timeout: None,
            no_decompression: false,
            force_no_timeout: false,
            force_addr: None,
            protocol_hint: ProtocolHint::Auto,
            fragment,
        }
    }

    pub(crate) fn new_owned(
        client: HttpEngineLocal<R, C>,
        method: Method,
        uri: Uri,
        fragment: Option<String>,
    ) -> Self {
        Self {
            client: EngineRef::Owned(Box::new(client)),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            version: None,
            timeout: None,
            connect_timeout: None,
            read_timeout: None,
            write_timeout: None,
            no_decompression: false,
            force_no_timeout: false,
            force_addr: None,
            protocol_hint: ProtocolHint::Auto,
            fragment,
        }
    }

    /// Add a header to the request.
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Add a header from string values.
    pub fn header_str(mut self, name: &str, value: &str) -> Result<Self, Error> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| Error::InvalidHeader(format!("invalid header name: {e}")))?;
        let value: HeaderValue = value
            .parse()
            .map_err(|e| Error::InvalidHeader(format!("invalid header value: {e}")))?;
        self.headers.insert(name, value);
        Ok(self)
    }

    /// Set multiple headers at once.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    /// Set a Bearer token Authorization header.
    pub fn bearer_auth(mut self, token: &str) -> Self {
        let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) else {
            return self;
        };
        self.headers.insert(AUTHORIZATION, value);
        self
    }

    /// Set a Basic Authorization header.
    pub fn basic_auth(mut self, username: &str, password: Option<&str>) -> Self {
        use base64::engine::{Engine, general_purpose::STANDARD};
        let credentials = match password {
            Some(pw) => format!("{username}:{pw}"),
            None => format!("{username}:"),
        };
        let encoded = STANDARD.encode(credentials);
        let Ok(value) = HeaderValue::from_str(&format!("Basic {encoded}")) else {
            return self;
        };
        self.headers.insert(AUTHORIZATION, value);
        self
    }

    /// Append query parameters to the URL.
    pub fn query(mut self, params: &[(&str, &str)]) -> Self {
        use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
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
        let sep = if self.uri.query().is_some() { '&' } else { '?' };
        for (i, (key, value)) in params.iter().enumerate() {
            let s = if i == 0 { sep } else { '&' };
            let k = utf8_percent_encode(key, QUERY_ENCODE);
            let v = utf8_percent_encode(value, QUERY_ENCODE);
            let _ = write!(uri_str, "{s}{k}={v}");
        }
        if let Ok(new_uri) = uri_str.parse() {
            self.uri = new_uri;
        }
        self
    }

    #[cfg(feature = "json")]
    /// Append query parameters from a serializable value.
    pub fn query_serde(mut self, params: &impl serde::Serialize) -> Result<Self, Error> {
        let query_string =
            serde_urlencoded::to_string(params).map_err(|e| Error::Other(Box::new(e)))?;
        if !query_string.is_empty() {
            let mut uri_str = self.uri.to_string();
            let sep = if self.uri.query().is_some() { '&' } else { '?' };
            let _ = write!(uri_str, "{sep}{query_string}");
            if let Ok(new_uri) = uri_str.parse() {
                self.uri = new_uri;
            }
        }
        Ok(self)
    }

    /// Set the request body from bytes.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(RequestBody::Buffered(body.into()));
        self
    }

    /// Set a streaming request body.
    pub fn body_stream(mut self, body: RequestBodySend) -> Self {
        self.body = Some(RequestBody::Streaming(body));
        self
    }

    #[cfg(feature = "json")]
    /// Serialize a value as JSON and set it as the request body.
    pub fn json(mut self, value: &impl serde::Serialize) -> Result<Self, Error> {
        let bytes = serde_json::to_vec(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.headers
            .entry(http::header::CONTENT_TYPE)
            .or_insert_with(|| HeaderValue::from_static("application/json"));
        self.body = Some(RequestBody::Buffered(bytes.into()));
        Ok(self)
    }

    /// Set a form-encoded request body from key-value pairs.
    pub fn form(mut self, params: &[(&str, &str)]) -> Self {
        use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
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
        for (i, (key, value)) in params.iter().enumerate() {
            if i > 0 {
                encoded.push('&');
            }
            let k = utf8_percent_encode(key, FORM_ENCODE);
            let v = utf8_percent_encode(value, FORM_ENCODE);
            let _ = write!(encoded, "{k}={v}");
        }
        let encoded = encoded.replace("%20", "+");
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Some(RequestBody::Buffered(encoded.into()));
        self
    }

    #[cfg(feature = "json")]
    /// Set a form-encoded request body from a serializable value.
    pub fn form_serde(mut self, value: &impl serde::Serialize) -> Result<Self, Error> {
        let encoded = serde_urlencoded::to_string(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Some(RequestBody::Buffered(encoded.into()));
        Ok(self)
    }

    /// Set a multipart form body.
    pub fn multipart(mut self, multipart: crate::multipart::Multipart) -> Self {
        let ct = multipart.content_type();
        let Ok(value) = HeaderValue::from_str(&ct) else {
            return self;
        };
        self.headers.insert(http::header::CONTENT_TYPE, value);
        if multipart.has_streaming_parts() {
            self.body = Some(RequestBody::Streaming(multipart.into_streaming_body()));
        } else {
            self.body = Some(RequestBody::Buffered(multipart.into_bytes()));
        }
        self
    }

    /// Set the HTTP version.
    pub fn version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Set a request timeout (overrides the client default).
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Set a timeout for establishing this request's connection.
    ///
    /// This overrides the client's default connect timeout. The request or
    /// client overall timeout still bounds the whole request.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Set a timeout for writing (uploading) the request body.
    ///
    /// This overrides the client's default write timeout.
    pub fn write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = Some(timeout);
        self
    }

    /// Set a timeout for gaps between response body data chunks.
    ///
    /// This overrides the client's default read timeout for this request only.
    /// It applies to response body reads, not to waiting for response headers.
    /// If no body data arrives within this duration the request fails with
    /// [`Error::ReadTimeout`](crate::Error::ReadTimeout).
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = Some(timeout);
        self
    }

    /// Disable automatic response decompression for this request.
    ///
    /// Overrides the client default: the `Accept-Encoding` request header is not
    /// added and the response body is returned exactly as received (no gzip /
    /// brotli / zstd / deflate decoding).
    pub fn no_decompression(mut self) -> Self {
        self.no_decompression = true;
        self
    }

    /// Disable the overall request timeout for this specific request.
    pub fn no_timeout(mut self) -> Self {
        self.force_no_timeout = true;
        self
    }

    /// Use HTTP/2 prior knowledge (h2c) for this request.
    pub fn h2c_prior_knowledge(mut self) -> Self {
        self.protocol_hint = ProtocolHint::H2c;
        self
    }

    /// Force this request to connect to a specific address, bypassing DNS
    /// resolution.
    ///
    /// The `Host` header is still set from the request URL. Use this with
    /// [`HttpEngineLocal::resolve_all`] to implement custom load-balancing.
    pub fn force_addr(mut self, addr: std::net::SocketAddr) -> Self {
        self.force_addr = Some(addr);
        self
    }

    pub(crate) fn uri(&self) -> &Uri {
        &self.uri
    }

    /// Returns the request method.
    pub fn method_ref(&self) -> &Method {
        &self.method
    }

    /// Returns the request URL, after any base-URL resolution.
    pub fn url(&self) -> &Uri {
        &self.uri
    }

    /// Returns the headers configured on this builder so far.
    ///
    /// This reflects headers added via [`header`](Self::header) and friends, not
    /// the client's default headers (those are merged at send time).
    pub fn headers_ref(&self) -> &HeaderMap {
        &self.headers
    }

    /// Set upgrade headers for a WebSocket handshake.
    ///
    /// This sets `Connection: Upgrade`, `Upgrade: websocket`,
    /// `Sec-WebSocket-Version: 13`, a random `Sec-WebSocket-Key`, and forces HTTP/1.1.
    /// After calling `send()`, check for status 101 and call `response.upgrade()`.
    pub fn upgrade(mut self) -> Self {
        self.headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("Upgrade"),
        );
        self.headers
            .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
        self.headers.insert(
            http::header::SEC_WEBSOCKET_VERSION,
            HeaderValue::from_static("13"),
        );
        let key = super::generate_websocket_key();
        if let Ok(val) = HeaderValue::from_str(&key) {
            self.headers.insert(http::header::SEC_WEBSOCKET_KEY, val);
        }
        self.version = Some(Version::HTTP_11);
        self
    }

    /// Build the request without sending it.
    ///
    /// Returns the configured `http::Request` for inspection or manual sending.
    pub fn build(mut self) -> Result<http::Request<RequestBody>, Error> {
        let body = self
            .body
            .take()
            .unwrap_or(RequestBody::Buffered(Bytes::new()));
        let mut builder = http::Request::builder().method(self.method).uri(self.uri);
        if let Some(ver) = self.version {
            builder = builder.version(ver);
        }
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let mut req = builder.body(body).map_err(Error::Http)?;
        if self.protocol_hint != ProtocolHint::Auto {
            req.extensions_mut().insert(self.protocol_hint);
        }
        Ok(req)
    }

    /// Clone this request builder if the body is buffered.
    pub fn try_clone(&self) -> Option<Self> {
        let body = match &self.body {
            Some(b) => Some(b.try_clone()?),
            None => None,
        };
        Some(Self {
            client: self.client.try_clone_for_lifetime(),
            method: self.method.clone(),
            uri: self.uri.clone(),
            headers: self.headers.clone(),
            body,
            version: self.version,
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            no_decompression: self.no_decompression,
            force_no_timeout: self.force_no_timeout,
            force_addr: self.force_addr,
            protocol_hint: self.protocol_hint,
            fragment: self.fragment.clone(),
        })
    }

    /// Send the request and return the response.
    pub async fn send(self) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
        let effective_timeout = if self.force_no_timeout {
            None
        } else {
            self.timeout.or(self.client.core.timeout)
        };
        let effective_connect_timeout = self.connect_timeout.or(self.client.core.connect_timeout);
        let effective_write_timeout = self.write_timeout.or(self.client.core.write_timeout);
        let effective_read_timeout = self.read_timeout.or(self.client.core.read_timeout);

        let execute_fut = self.client.execute_local(
            self.method,
            self.uri,
            self.headers,
            self.body,
            self.version,
            effective_connect_timeout,
            effective_write_timeout,
            effective_read_timeout,
            self.no_decompression,
            self.force_addr,
            self.protocol_hint,
            self.fragment,
        );

        match effective_timeout {
            Some(duration) => {
                Timeout::WithTimeout {
                    future: execute_fut,
                    sleep: R::sleep(duration),
                }
                .await
            }
            None => execute_fut.await,
        }
    }
}

#[cfg(all(test, feature = "compio"))]
mod tests {
    use super::*;
    use crate::body::RequestBody;
    use crate::client::HttpEngineLocal;
    use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};

    fn test_client() -> HttpEngineLocal<CompioRuntime, TcpConnector> {
        HttpEngineLocal::new()
    }

    #[test]
    fn header_sets_value() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let rb = rb.header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
        let req = rb.build().unwrap();
        assert_eq!(req.headers().get("accept").unwrap(), "text/html");
    }

    #[test]
    fn builder_read_accessors() {
        let client = test_client();
        let rb = client
            .post_local("http://example.com/path?q=1")
            .unwrap()
            .header(http::header::ACCEPT, HeaderValue::from_static("text/html"));

        assert_eq!(rb.method_ref(), &http::Method::POST);
        assert_eq!(rb.url().to_string(), "http://example.com/path?q=1");
        assert_eq!(
            rb.headers_ref().get(http::header::ACCEPT).unwrap(),
            "text/html"
        );
    }

    #[test]
    fn headers_extends() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
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

    #[test]
    fn header_str_valid() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let rb = rb.header_str("x-custom", "value").unwrap();
        let req = rb.build().unwrap();
        assert_eq!(req.headers().get("x-custom").unwrap(), "value");
    }

    #[test]
    fn header_str_invalid_name() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let result = rb.header_str("invalid header\n", "value");
        assert!(result.is_err());
    }

    #[test]
    fn header_str_invalid_value() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let result = rb.header_str("x-custom", "bad\0value");
        assert!(result.is_err());
    }

    #[test]
    fn bearer_auth_sets_authorization() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
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

    #[test]
    fn basic_auth_with_password() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
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

    #[test]
    fn basic_auth_without_password() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let rb = rb.basic_auth("user", None);
        let req = rb.build().unwrap();
        assert!(req.headers().contains_key("authorization"));
    }

    #[test]
    fn query_appends_params() {
        let client = test_client();
        let rb = client.get_local("http://example.com/path").unwrap();
        let rb = rb.query(&[("key", "value"), ("a", "b")]);
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(uri.contains("key=value"));
        assert!(uri.contains("a=b"));
    }

    #[test]
    fn query_appends_to_existing() {
        let client = test_client();
        let rb = client
            .get_local("http://example.com/path?existing=1")
            .unwrap();
        let rb = rb.query(&[("new", "2")]);
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(uri.contains("existing=1"));
        assert!(uri.contains("new=2"));
    }

    #[test]
    fn query_encodes_special_chars() {
        let client = test_client();
        let rb = client.get_local("http://example.com/path").unwrap();
        let rb = rb.query(&[("key", "hello world"), ("tag", "a&b=c")]);
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(uri.contains("hello%20world"));
        assert!(uri.contains("a%26b%3Dc"));
    }

    #[cfg(feature = "json")]
    #[test]
    fn query_serde_appends_params() {
        #[derive(serde::Serialize)]
        struct Params {
            key: String,
            num: i32,
        }
        let client = test_client();
        let rb = client.get_local("http://example.com/").unwrap();
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
    #[test]
    fn query_serde_empty_struct() {
        #[derive(serde::Serialize)]
        struct Empty {}
        let client = test_client();
        let rb = client.get_local("http://example.com/path").unwrap();
        let rb = rb.query_serde(&Empty {}).unwrap();
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(!uri.contains('?'));
    }

    #[test]
    fn body_sets_buffered() {
        let client = test_client();
        let rb = client.post_local("http://example.com").unwrap();
        let rb = rb.body("hello");
        let req = rb.build().unwrap();
        match req.into_body() {
            RequestBody::Buffered(b) => assert_eq!(b, "hello"),
            _ => panic!("expected buffered"),
        }
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_sets_content_type_and_body() {
        let client = test_client();
        let rb = client.post_local("http://example.com").unwrap();
        let rb = rb.json(&serde_json::json!({"key": "value"})).unwrap();
        let req = rb.build().unwrap();
        assert_eq!(
            req.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_preserves_existing_content_type() {
        let client = test_client();
        let rb = client.post_local("http://example.com").unwrap();
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

    #[test]
    fn form_sets_content_type_and_body() {
        let client = test_client();
        let rb = client.post_local("http://example.com").unwrap();
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
    #[test]
    fn form_serde_sets_body() {
        #[derive(serde::Serialize)]
        struct FormData {
            name: String,
        }
        let client = test_client();
        let rb = client.post_local("http://example.com").unwrap();
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

    #[test]
    fn version_sets_http_version() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let rb = rb.version(Version::HTTP_11);
        let req = rb.build().unwrap();
        assert_eq!(req.version(), Version::HTTP_11);
    }

    #[test]
    fn build_default_body() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let req = rb.build().unwrap();
        assert_eq!(*req.method(), Method::GET);
    }

    #[test]
    fn try_clone_buffered() {
        let client = test_client();
        let rb = client
            .post_local("http://example.com")
            .unwrap()
            .body("data");
        let cloned = rb.try_clone();
        assert!(cloned.is_some());
    }

    #[test]
    fn try_clone_no_body() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let cloned = rb.try_clone();
        assert!(cloned.is_some());
    }

    #[test]
    fn try_clone_streaming_returns_none() {
        use http_body_util::BodyExt;
        let client = test_client();
        let rb = client.post_local("http://example.com").unwrap();
        let stream_body: crate::body::RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let rb = rb.body_stream(stream_body);
        let cloned = rb.try_clone();
        assert!(cloned.is_none());
    }

    #[test]
    fn upgrade_sets_headers() {
        let client = test_client();
        let rb = client.get_local("http://example.com").unwrap();
        let rb = rb.upgrade();
        let req = rb.build().unwrap();
        assert_eq!(req.headers().get("connection").unwrap(), "Upgrade");
        assert_eq!(req.headers().get("upgrade").unwrap(), "websocket");
        assert_eq!(req.headers().get("sec-websocket-version").unwrap(), "13");
        assert!(req.headers().get("sec-websocket-key").is_some());
        assert_eq!(req.version(), Version::HTTP_11);
    }

    #[test]
    fn multipart_sets_content_type() {
        let mp = crate::multipart::Multipart::new().text("field", "value");
        let client = test_client();
        let rb = client.post_local("http://example.com").unwrap();
        let rb = rb.multipart(mp);
        let req = rb.build().unwrap();
        let ct = req.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.starts_with("multipart/form-data; boundary="));
    }

    #[test]
    fn timeout_setter() {
        let client = test_client();
        let rb = client
            .get_local("http://example.com")
            .unwrap()
            .timeout(Duration::from_secs(5));
        let _req = rb.build().unwrap();
    }

    #[test]
    fn connect_timeout_setter() {
        let client = test_client();
        let rb = client
            .get_local("http://example.com")
            .unwrap()
            .connect_timeout(Duration::from_secs(2));
        assert_eq!(rb.connect_timeout, Some(Duration::from_secs(2)));
    }

    #[test]
    fn debug_request_builder_local() {
        let client = test_client();
        let rb = client.get_local("http://example.com/path").unwrap();
        let dbg = format!("{rb:?}");
        assert!(
            dbg.contains("RequestBuilderLocal"),
            "Debug output should contain struct name, got: {dbg}"
        );
        assert!(
            dbg.contains("GET"),
            "Debug output should contain method, got: {dbg}"
        );
    }

    #[test]
    fn force_addr_setter_local() {
        let client = test_client();
        let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let rb = client
            .get_local("http://example.com")
            .unwrap()
            .force_addr(addr);
        assert_eq!(rb.force_addr, Some(addr));
    }
}
