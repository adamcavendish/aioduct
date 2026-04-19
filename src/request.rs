use std::fmt::Write as _;
use std::marker::PhantomData;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};

use crate::body::RequestBody;
use crate::client::Client;
use crate::error::{Error, HyperBody, Result};
use crate::response::Response;
use crate::retry::RetryConfig;
use crate::runtime::Runtime;
use crate::timeout::Timeout;

/// Builder for configuring and sending an HTTP request.
pub struct RequestBuilder<'a, R: Runtime> {
    client: &'a Client<R>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<RequestBody>,
    version: Option<Version>,
    timeout: Option<Duration>,
    retry: Option<RetryConfig>,
    _runtime: PhantomData<R>,
}

impl<'a, R: Runtime> RequestBuilder<'a, R> {
    pub(crate) fn new(client: &'a Client<R>, method: Method, uri: Uri) -> Self {
        Self {
            client,
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
    pub fn header_str(mut self, name: &str, value: &str) -> Result<Self> {
        let name: HeaderName = name.parse().map_err(|e| Error::Other(Box::new(e)))?;
        let value: HeaderValue = value.parse().map_err(|e| Error::Other(Box::new(e)))?;
        self.headers.insert(name, value);
        Ok(self)
    }

    /// Set a Bearer token Authorization header.
    pub fn bearer_auth(mut self, token: &str) -> Self {
        let value = HeaderValue::from_str(&format!("Bearer {token}")).expect("valid bearer token");
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
        let value =
            HeaderValue::from_str(&format!("Basic {encoded}")).expect("valid basic auth header");
        self.headers.insert(AUTHORIZATION, value);
        self
    }

    /// Append URL query parameters.
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
            .add(b'+');

        let mut uri_str = self.uri.to_string();
        let has_query = self.uri.query().is_some();
        for (i, (key, val)) in params.iter().enumerate() {
            let sep = if i == 0 && !has_query { '?' } else { '&' };
            let key = utf8_percent_encode(key, QUERY_ENCODE);
            let val = utf8_percent_encode(val, QUERY_ENCODE);
            write!(uri_str, "{sep}{key}={val}").unwrap();
        }
        if let Ok(new_uri) = uri_str.parse() {
            self.uri = new_uri;
        }
        self
    }

    /// Set a buffered request body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(RequestBody::Buffered(body.into()));
        self
    }

    /// Set a streaming request body.
    pub fn body_stream(mut self, body: HyperBody) -> Self {
        self.body = Some(RequestBody::Streaming(body));
        self
    }

    #[cfg(feature = "json")]
    /// Serialize a value as JSON and set it as the request body.
    pub fn json(mut self, value: &impl serde::Serialize) -> Result<Self> {
        let bytes = serde_json::to_vec(value).map_err(|e| Error::Other(Box::new(e)))?;
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        self.body = Some(RequestBody::Buffered(bytes.into()));
        Ok(self)
    }

    /// Set a URL-encoded form body.
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
            write!(encoded, "{k}={v}").unwrap();
        }
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Some(RequestBody::Buffered(encoded.into()));
        self
    }

    /// Set a multipart/form-data body.
    pub fn multipart(mut self, multipart: crate::multipart::Multipart) -> Self {
        let ct = multipart.content_type();
        let value = HeaderValue::from_str(&ct).expect("valid multipart content-type");
        self.headers.insert(http::header::CONTENT_TYPE, value);
        self.body = Some(RequestBody::Buffered(multipart.into_bytes()));
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
    /// This sets `Connection: Upgrade`, `Upgrade: websocket`, and forces HTTP/1.1.
    /// After calling `send()`, check for status 101 and call `response.upgrade()`.
    pub fn upgrade(mut self) -> Self {
        self.headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("Upgrade"),
        );
        self.headers
            .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
        self.version = Some(Version::HTTP_11);
        self
    }

    /// Send the request and return the response.
    pub async fn send(self) -> Result<Response> {
        let effective_retry = self.retry.as_ref().or(self.client.default_retry()).cloned();

        match effective_retry {
            Some(config) => self.send_with_retry(config).await,
            None => self.send_once().await,
        }
    }

    async fn send_once(self) -> Result<Response> {
        let effective_timeout = self.timeout.or(self.client.default_timeout());
        let execute_fut =
            self.client
                .execute(self.method, self.uri, self.headers, self.body, self.version);

        match effective_timeout {
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
        }
    }

    async fn send_with_retry(self, config: RetryConfig) -> Result<Response> {
        let effective_timeout = self.timeout.or(self.client.default_timeout());
        let mut last_error = None;
        let mut body = self.body;

        for attempt in 0..=config.max_retries {
            if attempt > 0 {
                let delay = config.delay_for_attempt(attempt - 1);
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
                        && resp.status().is_server_error()
                        && attempt < config.max_retries
                    {
                        last_error = Some(Error::Other(
                            format!("server error: {}", resp.status()).into(),
                        ));
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    if attempt < config.max_retries && crate::retry::is_retryable_error(&e) {
                        last_error = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Err(last_error.unwrap_or(Error::Other("retry exhausted".into())))
    }
}
