use std::fmt::Write as _;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};

use crate::body::{RequestBody, RequestBodySend};
use crate::client::{
    BodyReplayability, FinalizedRequestState, HttpEngineLocal, ReplayReason, RequestReplayPolicy,
};
use crate::error::{BuilderError, Error};
use crate::observer::{self, RequestEvent, RequestPhase, RetryKind};
use crate::pool::ProtocolHint;
use crate::response::Response;
use crate::retry::RetryConfig;
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
    automatic_content_digest: Option<bool>,
    builder_error: Option<BuilderError>,
    /// Original URL fragment from the user-provided URL string.
    /// Preserved across redirects per RFC 7231 Section 7.1.2.
    fragment: Option<String>,
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> std::fmt::Debug for RequestBuilderLocal<'_, R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestBuilderLocal")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("body", &self.body)
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
            automatic_content_digest: None,
            builder_error: None,
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
            automatic_content_digest: None,
            builder_error: None,
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
    /// Append query parameters from a serializable value.
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

    /// Set the request body from bytes.
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

    /// Set a form-encoded request body from key-value pairs.
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

    /// Set a multipart form body.
    ///
    /// If another body-setter method is called after this, the body will be
    /// silently replaced.
    pub fn multipart(mut self, multipart: crate::multipart::Multipart) -> Self {
        let ct = multipart.content_type();
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
    /// The `Host` header and TLS server name are still taken from the request
    /// URL. When a proxy route is selected, the proxy hops are resolved
    /// normally and this address overrides only the final tunnel destination.
    /// Use this with [`HttpEngineLocal::resolve_all`] to implement custom
    /// load-balancing.
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

    /// Returns the request body configured on this builder, if any.
    ///
    /// Returns `None` if no body-setter method has been called.
    pub fn body_ref(&self) -> Option<&RequestBody> {
        self.body.as_ref()
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
            automatic_content_digest: self.automatic_content_digest,
            builder_error: self.builder_error.clone(),
            fragment: self.fragment.clone(),
        })
    }

    /// Send the request and return the response.
    pub async fn send(mut self) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
        if let Some(error) = self.builder_error.take() {
            return Err(error.into_error());
        }
        let retry = self.client.core.retry.clone();
        match retry {
            Some(config) => Box::pin(self.send_with_retry(config)).await,
            None => Box::pin(self.send_once()).await,
        }
    }

    async fn send_once(self) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
        let effective_timeout = if self.force_no_timeout {
            None
        } else {
            self.timeout.or(self.client.core.timeout)
        };
        let effective_connect_timeout = self.connect_timeout.or(self.client.core.connect_timeout);
        let effective_write_timeout = self.write_timeout.or(self.client.core.write_timeout);
        let effective_read_timeout = self.read_timeout.or(self.client.core.read_timeout);
        let automatic_content_digest = self
            .automatic_content_digest
            .unwrap_or(self.client.core.automatic_content_digest);
        let method = self.method.clone();
        let uri = self.uri.clone();

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
            automatic_content_digest,
            self.fragment,
            None,
        );

        let result = match effective_timeout {
            Some(duration) => {
                Timeout::WithTimeout {
                    future: execute_fut,
                    sleep: R::sleep(duration),
                }
                .await
            }
            None => execute_fut.await,
        };
        if let Err(ref error) = result
            && !self.client.core.middleware.is_empty()
        {
            self.client
                .core
                .middleware
                .apply_error(error, &uri, &method);
        }
        result
    }

    async fn send_with_retry(
        mut self,
        config: RetryConfig,
    ) -> Result<Response<crate::body::ResponseBodyLocal>, Error> {
        let retry_start = crate::clock::Instant::now();
        let effective_timeout = if self.force_no_timeout {
            None
        } else {
            self.timeout.or(self.client.core.timeout)
        };
        let effective_connect_timeout = self.connect_timeout.or(self.client.core.connect_timeout);
        let effective_write_timeout = self.write_timeout.or(self.client.core.write_timeout);
        let effective_read_timeout = self.read_timeout.or(self.client.core.read_timeout);
        let automatic_content_digest = self
            .automatic_content_digest
            .unwrap_or(self.client.core.automatic_content_digest);
        let initial_body_replayability = match self.body.as_ref() {
            Some(RequestBody::Buffered(_)) => BodyReplayability::Replayable,
            Some(RequestBody::Streaming(_)) => BodyReplayability::OneShot,
            None => BodyReplayability::Empty,
        };
        let mut body = self.body.take();
        let mut retry_after_delay = None;
        let finalized_request = std::sync::Mutex::new(FinalizedRequestState::new(
            self.method.clone(),
            initial_body_replayability,
            config.max_retries,
            config.budget.clone(),
        ));
        let mut attempt = 0;

        loop {
            if attempt > 0 {
                let delay = retry_after_delay
                    .take()
                    .unwrap_or_else(|| config.delay_for_attempt(attempt - 1));
                R::sleep(delay).await;
            }

            let body_for_attempt = match &mut body {
                Some(RequestBody::Buffered(bytes)) => Some(RequestBody::Buffered(bytes.clone())),
                Some(RequestBody::Streaming(_)) => body.take(),
                None => None,
            };
            let execute_fut = self.client.execute_local(
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
                Some(&finalized_request),
            );
            let result = match effective_timeout {
                Some(duration) => {
                    Timeout::WithTimeout {
                        future: execute_fut,
                        sleep: R::sleep(duration),
                    }
                    .await
                }
                None => execute_fut.await,
            };
            let (wire_method, wire_uri, replay_policy, has_replay_snapshot, current_attempt) = {
                let finalized_request = finalized_request
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                (
                    finalized_request.method().clone(),
                    finalized_request
                        .effective_uri()
                        .cloned()
                        .unwrap_or_else(|| self.uri.clone()),
                    RequestReplayPolicy::new(finalized_request.method(), finalized_request.body()),
                    finalized_request.has_replay_snapshot(),
                    finalized_request.retry_attempt(),
                )
            };
            attempt = current_attempt;

            match result {
                Ok(resp) => {
                    let default_should_retry = config.retry_on_status
                        && crate::retry::is_retryable_status(resp.status())
                        && crate::retry::is_idempotent(&wire_method);
                    let should_retry =
                        match config.classify_status(resp.status(), &wire_method, attempt) {
                            crate::retry::RetryDecision::Retry => true,
                            crate::retry::RetryDecision::DoNotRetry => false,
                            crate::retry::RetryDecision::UseDefault => default_should_retry,
                        };
                    let can_retry = has_replay_snapshot
                        && replay_policy.permits(ReplayReason::Configured {
                            method_authorized: should_retry,
                        });
                    let next_attempt = if can_retry {
                        finalized_request
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .try_start_configured_retry()
                    } else {
                        None
                    };
                    if let Some(next_attempt) = next_attempt {
                        retry_after_delay = crate::retry::parse_retry_after(resp.headers());
                        let error = Error::Other(format!("server error: {}", resp.status()).into());
                        self.observe_retry_failure(
                            &wire_method,
                            &wire_uri,
                            &error,
                            retry_start.elapsed(),
                        );
                        self.observe_retrying(
                            &wire_method,
                            &wire_uri,
                            &error,
                            next_attempt,
                            config.max_retries,
                            retry_after_delay.unwrap_or_else(|| config.delay_for_attempt(attempt)),
                        );
                        if !self.client.core.middleware.is_empty() {
                            self.client.core.middleware.apply_retry(
                                &error,
                                &wire_uri,
                                &wire_method,
                                next_attempt,
                            );
                        }
                        attempt = next_attempt;
                        continue;
                    }
                    if finalized_request
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .retry_budget_denied()
                    {
                        return Ok(resp);
                    }
                    if let Some(ref budget) = config.budget {
                        budget.deposit();
                    }
                    return Ok(resp);
                }
                Err(error) => {
                    let default_should_retry = crate::retry::is_retryable_error(&error)
                        && crate::retry::is_idempotent(&wire_method);
                    let should_retry = match config.classify_error(&error, &wire_method, attempt) {
                        crate::retry::RetryDecision::Retry => true,
                        crate::retry::RetryDecision::DoNotRetry => false,
                        crate::retry::RetryDecision::UseDefault => default_should_retry,
                    };
                    let can_retry = has_replay_snapshot
                        && replay_policy.permits(ReplayReason::Configured {
                            method_authorized: should_retry,
                        });
                    let next_attempt = if can_retry {
                        finalized_request
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .try_start_configured_retry()
                    } else {
                        None
                    };
                    if let Some(next_attempt) = next_attempt {
                        self.observe_retry_failure(
                            &wire_method,
                            &wire_uri,
                            &error,
                            retry_start.elapsed(),
                        );
                        self.observe_retrying(
                            &wire_method,
                            &wire_uri,
                            &error,
                            next_attempt,
                            config.max_retries,
                            config.delay_for_attempt(attempt),
                        );
                        if !self.client.core.middleware.is_empty() {
                            self.client.core.middleware.apply_retry(
                                &error,
                                &wire_uri,
                                &wire_method,
                                next_attempt,
                            );
                        }
                        attempt = next_attempt;
                        continue;
                    }
                    self.apply_error_middleware(&wire_method, &wire_uri, &error);
                    return Err(error);
                }
            }
        }
    }

    fn observe_retry_failure(&self, method: &Method, uri: &Uri, error: &Error, elapsed: Duration) {
        if let Some(ref observer) = self.client.core.observer {
            observer.on_event(&RequestEvent {
                method: method.clone(),
                uri: uri.clone(),
                phase: RequestPhase::Failed {
                    error: error.to_string(),
                    retry: RetryKind::Explicit,
                    elapsed,
                },
                at: observer::Instant::now(),
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_retrying(
        &self,
        method: &Method,
        uri: &Uri,
        error: &Error,
        attempt: u32,
        max_retries: u32,
        backoff: Duration,
    ) {
        if let Some(ref observer) = self.client.core.observer {
            observer.on_event(&RequestEvent {
                method: method.clone(),
                uri: uri.clone(),
                phase: RequestPhase::Retrying {
                    reason: error.to_string(),
                    attempt,
                    max_retries,
                    backoff,
                },
                at: observer::Instant::now(),
            });
        }
    }

    fn apply_error_middleware(&self, method: &Method, uri: &Uri, error: &Error) {
        if !self.client.core.middleware.is_empty() {
            self.client.core.middleware.apply_error(error, uri, method);
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
