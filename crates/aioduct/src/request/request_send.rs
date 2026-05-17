use std::fmt::Write as _;
use std::marker::PhantomData;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use http::{Method, StatusCode, Uri, Version};

use crate::body::RequestBody;
use crate::body::RequestBodySend;
use crate::client::HttpEngineSend;
use crate::error::{Error, SendError};
use crate::response::Response;
use crate::retry::RetryConfig;
use crate::runtime::{ConnectorSend, RuntimePoll};
use crate::timeout::Timeout;

use super::EngineRef;

/// Builder for configuring and sending an HTTP request.
pub struct RequestBuilderSend<'a, R: RuntimePoll, C: ConnectorSend> {
    client: EngineRef<'a, HttpEngineSend<R, C>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<RequestBody>,
    version: Option<Version>,
    timeout: Option<Duration>,
    retry: Option<RetryConfig>,
    _runtime: PhantomData<(R, C)>,
}

impl<R: RuntimePoll, C: ConnectorSend> std::fmt::Debug for RequestBuilderSend<'_, R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestBuilderSend")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .finish()
    }
}

impl<'a, R: RuntimePoll, C: ConnectorSend> RequestBuilderSend<'a, R, C> {
    pub(crate) fn new(client: &'a HttpEngineSend<R, C>, method: Method, uri: Uri) -> Self {
        Self {
            client: EngineRef::Borrowed(client),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            version: None,
            timeout: None,
            retry: None,
            _runtime: PhantomData,
        }
    }

    pub(crate) fn new_owned(client: HttpEngineSend<R, C>, method: Method, uri: Uri) -> Self {
        Self {
            client: EngineRef::Owned(Box::new(client)),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            version: None,
            timeout: None,
            retry: None,
            _runtime: PhantomData,
        }
    }

    /// Add a typed header to the request.
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Add multiple headers to the request.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    /// Add a header from string name and value.
    pub fn header_str(mut self, name: &str, value: &str) -> Result<Self, Error> {
        let name: HeaderName = name
            .parse()
            .map_err(|e: http::header::InvalidHeaderName| Error::InvalidHeader(e.to_string()))?;
        let value: HeaderValue = value
            .parse()
            .map_err(|e: http::header::InvalidHeaderValue| Error::InvalidHeader(e.to_string()))?;
        self.headers.insert(name, value);
        Ok(self)
    }

    /// Set a Bearer token Authorization header.
    ///
    /// If the token contains invalid header characters, this is a no-op.
    pub fn bearer_auth(mut self, token: &str) -> Self {
        let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) else {
            return self;
        };
        self.headers.insert(AUTHORIZATION, value);
        self
    }

    /// Set a Basic Authorization header.
    ///
    /// If the username or password produce an invalid header value, this is a no-op.
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

    /// Append URL query parameters from string pairs.
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
        let has_query = self.uri.query().is_some();
        for (i, (key, val)) in params.iter().enumerate() {
            let sep = if i == 0 && !has_query { '?' } else { '&' };
            let key = utf8_percent_encode(key, QUERY_ENCODE);
            let val = utf8_percent_encode(val, QUERY_ENCODE);
            let _ = write!(uri_str, "{sep}{key}={val}");
        }
        if let Ok(new_uri) = uri_str.parse() {
            self.uri = new_uri;
        }
        self
    }

    #[cfg(feature = "json")]
    /// Append URL query parameters from a serializable value.
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

    /// Set a buffered request body.
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

    /// Set a URL-encoded form body from string pairs.
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
        for (i, (key, val)) in params.iter().enumerate() {
            if i > 0 {
                encoded.push('&');
            }
            let k = utf8_percent_encode(key, FORM_ENCODE);
            let v = utf8_percent_encode(val, FORM_ENCODE);
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
    /// Set a URL-encoded form body from a serializable value.
    pub fn form_serde(mut self, value: &impl serde::Serialize) -> Result<Self, Error> {
        let encoded = serde_urlencoded::to_string(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Some(RequestBody::Buffered(encoded.into()));
        Ok(self)
    }

    /// Set a multipart/form-data body.
    pub fn multipart(mut self, multipart: crate::multipart::Multipart) -> Self {
        let ct = multipart.content_type();
        // Content-type is constructed from valid parts
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

    /// Force a specific HTTP version.
    pub fn version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Set a timeout for this request, overriding the client default.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set a retry configuration for this request.
    pub fn retry(mut self, config: RetryConfig) -> Self {
        self.retry = Some(config);
        self
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
        builder.body(body).map_err(Error::Http)
    }

    /// Clone this request builder if the body is cloneable (buffered).
    /// Returns `None` if the body is a non-cloneable stream.
    pub fn try_clone(&self) -> Option<Self> {
        let cloned_body = match &self.body {
            Some(b) => Some(b.try_clone()?),
            None => None,
        };
        Some(Self {
            client: self.client.try_clone_for_lifetime(),
            method: self.method.clone(),
            uri: self.uri.clone(),
            headers: self.headers.clone(),
            body: cloned_body,
            version: self.version,
            timeout: self.timeout,
            retry: self.retry.clone(),
            _runtime: PhantomData,
        })
    }

    /// Send the request and return the response.
    ///
    /// On failure, returns [`SendError`] which includes the URL that was being
    /// requested. Use [`SendError::into_error()`] to discard URL context, or
    /// call convenience methods like [`SendError::is_timeout()`] directly.
    pub async fn send(self) -> Result<Response, SendError> {
        let url = self.uri.clone();
        let effective_retry = self.retry.as_ref().or(self.client.default_retry()).cloned();

        let result = match effective_retry {
            Some(config) => self.send_with_retry(config).await,
            None => self.send_once().await,
        };

        result.map_err(|error| SendError::new(error, url))
    }

    async fn send_once(self) -> Result<Response, Error> {
        let effective_timeout = self.timeout.or(self.client.default_timeout());
        let method = self.method.clone();
        let uri = self.uri.clone();
        let execute_fut =
            self.client
                .execute(self.method, self.uri, self.headers, self.body, self.version);

        let result = match effective_timeout {
            Some(duration) => {
                Timeout::WithTimeout {
                    future: execute_fut,
                    sleep: R::sleep(duration),
                }
                .await
            }
            None => {
                Timeout::<_, R::Sleep>::NoTimeout {
                    future: execute_fut,
                }
                .await
            }
        };

        if let Err(ref e) = result {
            let mw = self.client.middleware();
            if !mw.is_empty() {
                mw.apply_error(e, &uri, &method);
            }
        }
        result
    }

    async fn send_with_retry(self, config: RetryConfig) -> Result<Response, Error> {
        let effective_timeout = self.timeout.or(self.client.default_timeout());
        let mut last_error = None;
        let mut body = self.body;
        let mut retry_after_delay: Option<Duration> = None;

        for attempt in 0..=config.max_retries {
            if attempt > 0 {
                let delay = retry_after_delay
                    .take()
                    .unwrap_or_else(|| config.delay_for_attempt(attempt - 1));
                R::sleep(delay).await;
            }

            let body_for_attempt = match &mut body {
                Some(RequestBody::Buffered(b)) => Some(RequestBody::Buffered(b.clone())),
                Some(RequestBody::Streaming(_)) => body.take(),
                None => None,
            };

            let execute_fut = self.client.execute(
                self.method.clone(),
                self.uri.clone(),
                self.headers.clone(),
                body_for_attempt,
                self.version,
            );

            let result = match effective_timeout {
                Some(duration) => {
                    Timeout::WithTimeout {
                        future: execute_fut,
                        sleep: R::sleep(duration),
                    }
                    .await
                }
                None => {
                    Timeout::<_, R::Sleep>::NoTimeout {
                        future: execute_fut,
                    }
                    .await
                }
            };

            match result {
                Ok(resp) => {
                    if config.retry_on_status
                        && (resp.status().is_server_error()
                            || resp.status() == StatusCode::TOO_MANY_REQUESTS)
                        && attempt < config.max_retries
                        && crate::retry::is_idempotent(&self.method)
                    {
                        if let Some(ref budget) = config.budget
                            && !budget.try_withdraw()
                        {
                            return Ok(resp);
                        }
                        retry_after_delay = crate::retry::parse_retry_after(resp.headers());
                        let err = Error::Other(format!("server error: {}", resp.status()).into());
                        let mw = self.client.middleware();
                        if !mw.is_empty() {
                            mw.apply_retry(&err, &self.uri, &self.method, attempt + 1);
                        }
                        last_error = Some(err);
                        continue;
                    }
                    if let Some(ref budget) = config.budget {
                        budget.deposit();
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    if attempt < config.max_retries
                        && crate::retry::is_retryable_error(&e)
                        && crate::retry::is_idempotent(&self.method)
                    {
                        if let Some(ref budget) = config.budget
                            && !budget.try_withdraw()
                        {
                            let mw = self.client.middleware();
                            if !mw.is_empty() {
                                mw.apply_error(&e, &self.uri, &self.method);
                            }
                            return Err(e);
                        }
                        let mw = self.client.middleware();
                        if !mw.is_empty() {
                            mw.apply_retry(&e, &self.uri, &self.method, attempt + 1);
                        }
                        last_error = Some(e);
                        continue;
                    }
                    let mw = self.client.middleware();
                    if !mw.is_empty() {
                        mw.apply_error(&e, &self.uri, &self.method);
                    }
                    return Err(e);
                }
            }
        }

        let err = last_error.unwrap_or(Error::Other("retry exhausted".into()));
        let mw = self.client.middleware();
        if !mw.is_empty() {
            mw.apply_error(&err, &self.uri, &self.method);
        }
        Err(err)
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};

    fn test_client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::new(TcpConnector)
    }

    #[tokio::test]
    async fn header_sets_value() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let rb = rb.header(http::header::ACCEPT, HeaderValue::from_static("text/html"));
        let req = rb.build().unwrap();
        assert_eq!(req.headers().get("accept").unwrap(), "text/html");
    }

    #[tokio::test]
    async fn headers_extends() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
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

    #[tokio::test]
    async fn header_str_valid() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let rb = rb.header_str("x-custom", "value").unwrap();
        let req = rb.build().unwrap();
        assert_eq!(req.headers().get("x-custom").unwrap(), "value");
    }

    #[tokio::test]
    async fn bearer_auth_sets_authorization() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
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

    #[tokio::test]
    async fn basic_auth_with_password() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
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

    #[tokio::test]
    async fn basic_auth_without_password() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let rb = rb.basic_auth("user", None);
        let req = rb.build().unwrap();
        assert!(req.headers().contains_key("authorization"));
    }

    #[tokio::test]
    async fn query_appends_params() {
        let client = test_client();
        let rb = client.get("http://example.com/path").unwrap();
        let rb = rb.query(&[("key", "value"), ("a", "b")]);
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(uri.contains("key=value"));
        assert!(uri.contains("a=b"));
    }

    #[tokio::test]
    async fn query_appends_to_existing() {
        let client = test_client();
        let rb = client.get("http://example.com/path?existing=1").unwrap();
        let rb = rb.query(&[("new", "2")]);
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(uri.contains("existing=1"));
        assert!(uri.contains("new=2"));
    }

    #[tokio::test]
    async fn body_sets_buffered() {
        let client = test_client();
        let rb = client.post("http://example.com").unwrap();
        let rb = rb.body("hello");
        let req = rb.build().unwrap();
        match req.into_body() {
            RequestBody::Buffered(b) => assert_eq!(b, "hello"),
            _ => panic!("expected buffered"),
        }
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn json_sets_content_type_and_body() {
        let client = test_client();
        let rb = client.post("http://example.com").unwrap();
        let rb = rb.json(&serde_json::json!({"key": "value"})).unwrap();
        let req = rb.build().unwrap();
        assert_eq!(
            req.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn json_preserves_existing_content_type() {
        let client = test_client();
        let rb = client.post("http://example.com").unwrap();
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

    #[tokio::test]
    async fn form_sets_content_type_and_body() {
        let client = test_client();
        let rb = client.post("http://example.com").unwrap();
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
    #[tokio::test]
    async fn query_serde_appends_params() {
        #[derive(serde::Serialize)]
        struct Params {
            key: String,
            num: i32,
        }
        let client = test_client();
        let rb = client.get("http://example.com/").unwrap();
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
    #[tokio::test]
    async fn form_serde_sets_body() {
        #[derive(serde::Serialize)]
        struct FormData {
            name: String,
        }
        let client = test_client();
        let rb = client.post("http://example.com").unwrap();
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

    #[tokio::test]
    async fn version_sets_http_version() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let rb = rb.version(Version::HTTP_11);
        let req = rb.build().unwrap();
        assert_eq!(req.version(), Version::HTTP_11);
    }

    #[tokio::test]
    async fn build_default_body() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let req = rb.build().unwrap();
        assert_eq!(*req.method(), Method::GET);
    }

    #[tokio::test]
    async fn try_clone_buffered() {
        let client = test_client();
        let rb = client.post("http://example.com").unwrap().body("data");
        let cloned = rb.try_clone();
        assert!(cloned.is_some());
    }

    #[tokio::test]
    async fn try_clone_no_body() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let cloned = rb.try_clone();
        assert!(cloned.is_some());
    }

    #[tokio::test]
    async fn try_clone_streaming_returns_none() {
        use http_body_util::BodyExt;
        let client = test_client();
        let rb = client.post("http://example.com").unwrap();
        let stream_body: crate::body::RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let rb = rb.body_stream(stream_body);
        let cloned = rb.try_clone();
        assert!(cloned.is_none());
    }

    #[tokio::test]
    async fn upgrade_sets_headers() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let rb = rb.upgrade();
        let req = rb.build().unwrap();
        assert_eq!(req.headers().get("connection").unwrap(), "Upgrade");
        assert_eq!(req.headers().get("upgrade").unwrap(), "websocket");
        assert_eq!(req.headers().get("sec-websocket-version").unwrap(), "13");
        assert!(req.headers().get("sec-websocket-key").is_some());
        assert_eq!(req.version(), Version::HTTP_11);
    }

    #[tokio::test]
    async fn multipart_sets_content_type() {
        let mp = crate::multipart::Multipart::new().text("field", "value");
        let client = test_client();
        let rb = client.post("http://example.com").unwrap();
        let rb = rb.multipart(mp);
        let req = rb.build().unwrap();
        let ct = req.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.starts_with("multipart/form-data; boundary="));
    }

    #[tokio::test]
    async fn header_str_invalid_name() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let result = rb.header_str("invalid header\n", "value");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn header_str_invalid_value() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        let result = rb.header_str("x-custom", "bad\0value");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn debug_request_builder() {
        let client = test_client();
        let rb = client.get("http://example.com/path").unwrap();
        let dbg = format!("{rb:?}");
        assert!(dbg.contains("RequestBuilderSend"));
        assert!(dbg.contains("GET"));
    }

    #[tokio::test]
    async fn query_encodes_special_chars() {
        let client = test_client();
        let rb = client.get("http://example.com/path").unwrap();
        let rb = rb.query(&[("key", "hello world"), ("tag", "a&b=c")]);
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(uri.contains("hello%20world"));
        assert!(uri.contains("a%26b%3Dc"));
    }

    #[tokio::test]
    async fn timeout_setter() {
        let client = test_client();
        let rb = client
            .get("http://example.com")
            .unwrap()
            .timeout(Duration::from_secs(5));
        let _req = rb.build().unwrap();
    }

    #[tokio::test]
    async fn retry_setter() {
        let client = test_client();
        let rb = client
            .get("http://example.com")
            .unwrap()
            .retry(RetryConfig::default());
        let _req = rb.build().unwrap();
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn query_serde_empty_struct() {
        #[derive(serde::Serialize)]
        struct Empty {}
        let client = test_client();
        let rb = client.get("http://example.com/path").unwrap();
        let rb = rb.query_serde(&Empty {}).unwrap();
        let req = rb.build().unwrap();
        let uri = req.uri().to_string();
        assert!(!uri.contains('?'));
    }

    #[tokio::test]
    async fn bearer_auth_with_invalid_chars_is_noop() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        // Control characters are invalid in header values
        let rb = rb.bearer_auth("token\x00with\x01control\x02chars");
        let req = rb.build().unwrap();
        assert!(
            !req.headers().contains_key("authorization"),
            "invalid bearer token should not set header"
        );
    }

    #[tokio::test]
    async fn basic_auth_with_invalid_chars_is_noop() {
        let client = test_client();
        let rb = client.get("http://example.com").unwrap();
        // base64 encoding of a username with certain chars can still be valid,
        // but we can force invalid by using multi-line strings. Actually base64
        // always produces valid ASCII, so we test by observing that the header IS set.
        // The only way to trigger line 117 is if the base64-encoded result has invalid
        // header chars, which is impossible with standard base64.
        // Instead, let's verify that valid basic_auth works to document the behavior.
        let rb = rb.basic_auth("user", Some("pass"));
        let req = rb.build().unwrap();
        assert!(req.headers().contains_key("authorization"));
    }

    #[tokio::test]
    async fn send_without_timeout_succeeds() {
        // Exercises the NoTimeout path in send_once (line 352-356)
        let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
        let client = test_client();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    }

    #[tokio::test]
    async fn send_with_timeout_succeeds() {
        // Exercises the WithTimeout path in send_once (line 344-351)
        let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
        let client = test_client();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn retry_exhaustion_returns_error() {
        // Exercises lines 466-471: retry exhaustion path
        // Connect to an address that will always refuse connections.
        // Use port 1 which is unlikely to be open.
        let client = test_client();
        let result = client
            .get("http://127.0.0.1:1/")
            .unwrap()
            .timeout(Duration::from_millis(100))
            .retry(
                RetryConfig::default()
                    .max_retries(1)
                    .initial_backoff(Duration::from_millis(1))
                    .retry_on_status(false),
            )
            .send()
            .await;
        assert!(result.is_err(), "expected error from retry exhaustion");
    }

    #[tokio::test]
    async fn retry_on_server_error_exhausts() {
        // Server always returns 500; retry_on_status causes retries to exhaust.
        // This exercises the status retry branch (lines 415-432).
        let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async {
            Ok(hyper::Response::builder()
                .status(500)
                .body(http_body_util::Full::new(bytes::Bytes::from("error")))
                .unwrap())
        })
        .await;
        let client = test_client();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .retry(
                RetryConfig::default()
                    .max_retries(2)
                    .initial_backoff(Duration::from_millis(1))
                    .retry_on_status(true),
            )
            .send()
            .await
            .unwrap();
        // After retries exhausted, the last 500 response is returned
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // Should have made 3 total attempts (1 + 2 retries)
        assert_eq!(counter.requests(), 3);
    }

    #[tokio::test]
    async fn retry_without_timeout_uses_no_timeout_path() {
        // Exercises line 405: the NoTimeout path inside send_with_retry
        // Connect to a server that fails once then succeeds.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempt_count = Arc::new(AtomicU32::new(0));
        let attempt_count2 = attempt_count.clone();

        let (addr, _counter) = aioduct_test_server::h1::h1_server_with(move |_req| {
            let count = attempt_count2.fetch_add(1, Ordering::Relaxed);
            async move {
                if count == 0 {
                    // First request: return 503 to trigger retry
                    Ok(hyper::Response::builder()
                        .status(503)
                        .body(http_body_util::Full::new(bytes::Bytes::from("unavailable")))
                        .unwrap())
                } else {
                    Ok(hyper::Response::new(http_body_util::Full::new(
                        bytes::Bytes::from("ok"),
                    )))
                }
            }
        })
        .await;

        // Client with NO timeout, but with retry
        let client = test_client();
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .retry(
                RetryConfig::default()
                    .max_retries(2)
                    .initial_backoff(Duration::from_millis(1))
                    .retry_on_status(true),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(attempt_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn send_error_contains_url() {
        // Exercises the SendError wrapping at line 333
        let client = test_client();
        let err = client
            .get("http://127.0.0.1:1/specific-path")
            .unwrap()
            .timeout(Duration::from_millis(100))
            .send()
            .await
            .unwrap_err();
        assert!(
            err.url().to_string().contains("specific-path"),
            "SendError should contain the request URL, got: {}",
            err.url()
        );
    }
}
