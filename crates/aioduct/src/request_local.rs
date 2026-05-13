use std::time::Duration;

use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http::{Method, Uri, Version};

use crate::body::RequestBody;
use crate::client::HttpEngine;
use crate::error::Error;
use crate::response::Response;
use crate::runtime::{Connector, RuntimeLocal};
use crate::timeout::Timeout;

/// Builder for configuring and sending an HTTP request on a `!Send` runtime.
pub struct RequestBuilderLocal<'a, R: RuntimeLocal, C: Connector + Clone> {
    client: &'a HttpEngine<R, C>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<RequestBody>,
    version: Option<Version>,
    timeout: Option<Duration>,
}

impl<'a, R: RuntimeLocal, C: Connector + Clone> RequestBuilderLocal<'a, R, C> {
    pub(crate) fn new(client: &'a HttpEngine<R, C>, method: Method, uri: Uri) -> Self {
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

    /// Set the request body from bytes.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(RequestBody::Buffered(body.into()));
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

    /// Send the request and return the response.
    pub async fn send(self) -> Result<Response, Error> {
        let effective_timeout = self.timeout.or(self.client.timeout);

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
