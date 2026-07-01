use std::fmt::Write as _;
use std::marker::PhantomData;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};

use crate::body::RequestBody;
use crate::body::RequestBodySend;
use crate::client::HttpEngineSend;
use crate::error::{BuilderError, Error, SendError};
use crate::observer::{self, RequestEvent, RequestPhase, RetryKind};
use crate::pool::ProtocolHint;
use crate::response::Response;
use crate::retry::RetryConfig;
use crate::runtime::{ConnectorSend, RuntimePoll};
use crate::timeout::Timeout;

use super::EngineRef;

/// Builder for configuring and sending an HTTP request.
#[must_use = "a RequestBuilder does nothing unless you call `.send()` or `.build()`"]
pub struct RequestBuilderSend<'a, R: RuntimePoll, C: ConnectorSend> {
    client: EngineRef<'a, HttpEngineSend<R, C>>,
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
    retry: Option<RetryConfig>,
    force_addr: Option<std::net::SocketAddr>,
    protocol_hint: ProtocolHint,
    automatic_content_digest: Option<bool>,
    builder_error: Option<BuilderError>,
    /// Original URL fragment from the user-provided URL string.
    /// Preserved across redirects per RFC 7231 Section 7.1.2.
    fragment: Option<String>,
    _runtime: PhantomData<(R, C)>,
}

impl<R: RuntimePoll, C: ConnectorSend> std::fmt::Debug for RequestBuilderSend<'_, R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestBuilderSend")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("body", &self.body)
            .finish()
    }
}

impl<'a, R: RuntimePoll, C: ConnectorSend> RequestBuilderSend<'a, R, C> {
    pub(crate) fn new(
        client: &'a HttpEngineSend<R, C>,
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
            retry: None,
            force_addr: None,
            protocol_hint: ProtocolHint::Auto,
            automatic_content_digest: None,
            builder_error: None,
            fragment,
            _runtime: PhantomData,
        }
    }

    pub(crate) fn new_owned(
        client: HttpEngineSend<R, C>,
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
            retry: None,
            force_addr: None,
            protocol_hint: ProtocolHint::Auto,
            automatic_content_digest: None,
            builder_error: None,
            fragment,
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
    /// If the token contains invalid header characters, the builder records an
    /// error returned by [`build`](Self::build) or [`send`](Self::send).
    pub fn bearer_auth(mut self, token: &str) -> Self {
        let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) else {
            BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_header("invalid bearer token header value"),
            );
            return self;
        };
        self.headers.insert(AUTHORIZATION, value);
        self
    }

    /// Set a Basic Authorization header.
    ///
    /// If the username or password produce an invalid header value, the builder
    /// records an error returned by [`build`](Self::build) or [`send`](Self::send).
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
        match uri_str.parse() {
            Ok(new_uri) => self.uri = new_uri,
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_url(format!("failed to append query parameters: {e}")),
            ),
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
            let new_uri = uri_str.parse().map_err(|e| {
                Error::InvalidUrl(format!("failed to append query parameters: {e}"))
            })?;
            self.uri = new_uri;
        }
        Ok(self)
    }

    /// Set a buffered request body.
    ///
    /// If another body-setter method (`json()`, `form()`, `multipart()`, etc.)
    /// is called after this, the body will be silently replaced.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(RequestBody::Buffered(body.into()));
        self
    }

    /// Set a streaming request body.
    ///
    /// If another body-setter method (`body()`, `json()`, `form()`, etc.)
    /// is called after this, the body will be silently replaced.
    pub fn body_stream(mut self, body: RequestBodySend) -> Self {
        self.body = Some(RequestBody::Streaming(body));
        self
    }

    /// Override automatic `Content-Digest` generation for this request.
    ///
    /// When enabled, dispatch inserts a SHA-256 `Content-Digest` header for a
    /// buffered body that does not already have one. Streaming or
    /// middleware-replaced bodies are not buffered; set `Content-Digest`
    /// explicitly for those requests.
    pub fn automatic_content_digest(mut self, enable: bool) -> Self {
        self.automatic_content_digest = Some(enable);
        self
    }

    #[cfg(feature = "json")]
    /// Serialize a value as JSON and set it as the request body.
    ///
    /// If another body-setter method is called after this, the body will be
    /// silently replaced.
    pub fn json(mut self, value: &impl serde::Serialize) -> Result<Self, Error> {
        let bytes = serde_json::to_vec(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.headers
            .entry(http::header::CONTENT_TYPE)
            .or_insert_with(|| HeaderValue::from_static("application/json"));
        self.body = Some(RequestBody::Buffered(bytes.into()));
        Ok(self)
    }

    /// Set a URL-encoded form body from string pairs.
    ///
    /// If another body-setter method is called after this, the body will be
    /// silently replaced.
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
    ///
    /// If another body-setter method is called after this, the body will be
    /// silently replaced.
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
    ///
    /// If another body-setter method is called after this, the body will be
    /// silently replaced.
    pub fn multipart(mut self, multipart: crate::multipart::Multipart) -> Self {
        let ct = multipart.content_type();
        // Content-type is constructed from valid parts
        let Ok(value) = HeaderValue::from_str(&ct) else {
            BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_header("invalid multipart content-type header value"),
            );
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

    /// Returns the request body configured on this builder, if any.
    ///
    /// Returns `None` if no body-setter method has been called.
    pub fn body_ref(&self) -> Option<&RequestBody> {
        self.body.as_ref()
    }

    /// Set a timeout for this request, overriding the client default.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
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
    /// brotli / zstd / deflate decoding). Useful for proxying or inspecting raw
    /// compressed bytes.
    pub fn no_decompression(mut self) -> Self {
        self.no_decompression = true;
        self
    }

    /// Disable the overall request timeout for this specific request.
    ///
    /// Use when the client has a default timeout but this request
    /// (e.g., a long-running upload) should not be bounded.
    pub fn no_timeout(mut self) -> Self {
        self.force_no_timeout = true;
        self
    }

    /// Force this request to connect to a specific address, bypassing DNS
    /// resolution and Happy Eyeballs.
    ///
    /// The `Host` header is still set from the request URL. Use this with
    /// [`HttpEngineSend::resolve_all`] to implement custom load-balancing:
    ///
    /// ```ignore
    /// let addrs = client.resolve_all("my-svc.local", 8080).await?;
    /// let chosen = my_selector.select(&addrs);
    /// let resp = client.get("http://my-svc.local/api")
    ///     .force_addr(chosen)
    ///     .send().await?;
    /// ```
    pub fn force_addr(mut self, addr: std::net::SocketAddr) -> Self {
        self.force_addr = Some(addr);
        self
    }

    /// Use HTTP/2 prior knowledge (h2c) for this request.
    pub fn h2c_prior_knowledge(mut self) -> Self {
        self.protocol_hint = ProtocolHint::H2c;
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
        match HeaderValue::from_str(&key) {
            Ok(val) => {
                self.headers.insert(http::header::SEC_WEBSOCKET_KEY, val);
            }
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_header(format!("invalid websocket key header value: {e}")),
            ),
        }
        self.version = Some(Version::HTTP_11);
        self
    }

    /// Build the request without sending it.
    ///
    /// Returns the configured `http::Request` for inspection or manual sending.
    pub fn build(mut self) -> Result<http::Request<RequestBody>, Error> {
        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }
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
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
            no_decompression: self.no_decompression,
            force_no_timeout: self.force_no_timeout,
            retry: self.retry.clone(),
            force_addr: self.force_addr,
            protocol_hint: self.protocol_hint,
            automatic_content_digest: self.automatic_content_digest,
            builder_error: self.builder_error.clone(),
            fragment: self.fragment.clone(),
            _runtime: PhantomData,
        })
    }

    /// Send the request and return the response.
    ///
    /// On failure, returns [`SendError`] which includes the URL that was being
    /// requested. Use [`SendError::into_error()`] to discard URL context, or
    /// call convenience methods like [`SendError::is_timeout()`] directly.
    pub async fn send(self) -> Result<Response, SendError> {
        let mut this = self;
        let url = this.uri.clone();
        if let Some(error) = this.builder_error.take() {
            return Err(SendError::new(error.into_error(), url));
        }
        let self_ = this;
        let effective_retry = self_
            .retry
            .as_ref()
            .or(self_.client.default_retry())
            .cloned();

        let result = match effective_retry {
            Some(config) => self_.send_with_retry(config).await,
            None => self_.send_once().await,
        };

        result.map_err(|error| SendError::new(error, url))
    }

    async fn send_once(self) -> Result<Response, Error> {
        let effective_timeout = if self.force_no_timeout {
            None
        } else {
            self.timeout.or(self.client.default_timeout())
        };
        let effective_connect_timeout = self
            .connect_timeout
            .or(self.client.default_connect_timeout());
        let effective_write_timeout = self.write_timeout.or(self.client.default_write_timeout());
        let effective_read_timeout = self.read_timeout.or(self.client.default_read_timeout());
        let automatic_content_digest = self
            .automatic_content_digest
            .unwrap_or(self.client.core.automatic_content_digest);
        let method = self.method.clone();
        let uri = self.uri.clone();
        let execute_fut = self.client.execute_send(
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
            automatic_content_digest,
            self.fragment,
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

        if let Err(ref e) = result {
            let mw = self.client.middleware();
            if !mw.is_empty() {
                mw.apply_error(e, &uri, &method);
            }
        }
        result
    }

    async fn send_with_retry(self, config: RetryConfig) -> Result<Response, Error> {
        let retry_start = crate::clock::Instant::now();
        let effective_timeout = if self.force_no_timeout {
            None
        } else {
            self.timeout.or(self.client.default_timeout())
        };
        let effective_connect_timeout = self
            .connect_timeout
            .or(self.client.default_connect_timeout());
        let effective_write_timeout = self.write_timeout.or(self.client.default_write_timeout());
        let effective_read_timeout = self.read_timeout.or(self.client.default_read_timeout());
        let automatic_content_digest = self
            .automatic_content_digest
            .unwrap_or(self.client.core.automatic_content_digest);
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

            let execute_fut = self.client.execute_send(
                self.method.clone(),
                self.uri.clone(),
                self.headers.clone(),
                body_for_attempt,
                self.version,
                effective_connect_timeout,
                effective_write_timeout,
                effective_read_timeout,
                self.no_decompression,
                self.force_addr,
                self.protocol_hint,
                automatic_content_digest,
                self.fragment.clone(),
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
                    let default_should_retry = config.retry_on_status
                        && crate::retry::is_retryable_status(resp.status())
                        && crate::retry::is_idempotent(&self.method);
                    let should_retry =
                        match config.classify_status(resp.status(), &self.method, attempt) {
                            crate::retry::RetryDecision::Retry => true,
                            crate::retry::RetryDecision::DoNotRetry => false,
                            crate::retry::RetryDecision::UseDefault => default_should_retry,
                        };
                    if should_retry && attempt < config.max_retries {
                        if let Some(ref budget) = config.budget
                            && !budget.try_withdraw()
                        {
                            return Ok(resp);
                        }
                        retry_after_delay = crate::retry::parse_retry_after(resp.headers());
                        let err = Error::Other(format!("server error: {}", resp.status()).into());

                        if let Some(ref obs) = self.client.core.observer {
                            obs.on_event(&RequestEvent {
                                method: self.method.clone(),
                                uri: self.uri.clone(),
                                phase: RequestPhase::Failed {
                                    error: err.to_string(),
                                    retry: RetryKind::Explicit,
                                    elapsed: retry_start.elapsed(),
                                },
                                at: observer::Instant::now(),
                            });
                        }

                        let backoff =
                            retry_after_delay.unwrap_or_else(|| config.delay_for_attempt(attempt));
                        if let Some(ref obs) = self.client.core.observer {
                            obs.on_event(&RequestEvent {
                                method: self.method.clone(),
                                uri: self.uri.clone(),
                                phase: RequestPhase::Retrying {
                                    reason: err.to_string(),
                                    attempt: attempt + 1,
                                    max_retries: config.max_retries,
                                    backoff,
                                },
                                at: observer::Instant::now(),
                            });
                        }

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
                    let default_should_retry = crate::retry::is_retryable_error(&e)
                        && crate::retry::is_idempotent(&self.method);
                    let should_retry = match config.classify_error(&e, &self.method, attempt) {
                        crate::retry::RetryDecision::Retry => true,
                        crate::retry::RetryDecision::DoNotRetry => false,
                        crate::retry::RetryDecision::UseDefault => default_should_retry,
                    };
                    if should_retry && attempt < config.max_retries {
                        if let Some(ref budget) = config.budget
                            && !budget.try_withdraw()
                        {
                            let mw = self.client.middleware();
                            if !mw.is_empty() {
                                mw.apply_error(&e, &self.uri, &self.method);
                            }
                            return Err(e);
                        }

                        if let Some(ref obs) = self.client.core.observer {
                            obs.on_event(&RequestEvent {
                                method: self.method.clone(),
                                uri: self.uri.clone(),
                                phase: RequestPhase::Failed {
                                    error: e.to_string(),
                                    retry: RetryKind::Explicit,
                                    elapsed: retry_start.elapsed(),
                                },
                                at: observer::Instant::now(),
                            });
                        }

                        let backoff =
                            retry_after_delay.unwrap_or_else(|| config.delay_for_attempt(attempt));
                        if let Some(ref obs) = self.client.core.observer {
                            obs.on_event(&RequestEvent {
                                method: self.method.clone(),
                                uri: self.uri.clone(),
                                phase: RequestPhase::Retrying {
                                    reason: e.to_string(),
                                    attempt: attempt + 1,
                                    max_retries: config.max_retries,
                                    backoff,
                                },
                                at: observer::Instant::now(),
                            });
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

        if let Some(ref obs) = self.client.core.observer {
            obs.on_event(&RequestEvent {
                method: self.method.clone(),
                uri: self.uri.clone(),
                phase: RequestPhase::Failed {
                    error: err.to_string(),
                    retry: RetryKind::None,
                    elapsed: retry_start.elapsed(),
                },
                at: observer::Instant::now(),
            });
        }

        let mw = self.client.middleware();
        if !mw.is_empty() {
            mw.apply_error(&err, &self.uri, &self.method);
        }
        Err(err)
    }
}

#[cfg(all(test, feature = "tokio"))]
#[cfg(test)]
mod tests;
