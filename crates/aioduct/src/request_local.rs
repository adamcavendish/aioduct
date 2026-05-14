use std::fmt::Write as _;
use std::time::Duration;

use bytes::Bytes;
use http::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};

use crate::body::{RequestBody, RequestBoxBody};
use crate::client::HttpEngineLocal;
use crate::error::Error;
use crate::response::Response;
use crate::runtime::{Connector, RuntimeLocal};
use crate::timeout::Timeout;

/// Builder for configuring and sending an HTTP request on a `!Send` runtime.
pub struct RequestBuilderLocal<'a, R: RuntimeLocal, C: Connector + Clone> {
    client: &'a HttpEngineLocal<R, C>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<RequestBody>,
    version: Option<Version>,
    timeout: Option<Duration>,
}

impl<'a, R: RuntimeLocal, C: Connector + Clone> RequestBuilderLocal<'a, R, C> {
    pub(crate) fn new(client: &'a HttpEngineLocal<R, C>, method: Method, uri: Uri) -> Self {
        Self {
            client,
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            version: None,
            timeout: None,
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
            .map_err(|e| Error::InvalidUrl(format!("invalid header name: {e}")))?;
        let value: HeaderValue = value
            .parse()
            .map_err(|e| Error::InvalidUrl(format!("invalid header value: {e}")))?;
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
            .add(b'+');

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

    #[cfg(feature = "serde")]
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
    pub fn body_stream(mut self, body: RequestBoxBody) -> Self {
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
            .add(b'+');

        let mut encoded = String::new();
        for (i, (key, value)) in params.iter().enumerate() {
            if i > 0 {
                encoded.push('&');
            }
            let k = utf8_percent_encode(key, FORM_ENCODE);
            let v = utf8_percent_encode(value, FORM_ENCODE);
            let _ = write!(encoded, "{k}={v}");
        }
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        self.body = Some(RequestBody::Buffered(encoded.into()));
        self
    }

    #[cfg(feature = "serde")]
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

    /// Clone this request builder if the body is buffered.
    pub fn try_clone(&self) -> Option<Self> {
        let body = match &self.body {
            Some(b) => Some(b.try_clone()?),
            None => None,
        };
        Some(Self {
            client: self.client,
            method: self.method.clone(),
            uri: self.uri.clone(),
            headers: self.headers.clone(),
            body,
            version: self.version,
            timeout: self.timeout,
        })
    }

    /// Send the request and return the response.
    pub async fn send(self) -> Result<Response<crate::body::ResponseBoxLocalBody>, Error> {
        let effective_timeout = self.timeout.or(self.client.core.timeout);

        let execute_fut =
            self.client
                .execute_local(self.method, self.uri, self.headers, self.body, self.version);

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
