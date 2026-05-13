use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{HttpClient, RequestBuilderExt, ResponseExt};
use crate::body::ResponseBoxLocalBody;
use crate::client::HttpEngine;
use crate::error::{Error, SendError};
use crate::response::Response;
use crate::runtime::{Connector, RuntimeLocal};

/// An owned request builder for the Local runtime path.
pub struct OwnedRequestBuilderLocal<R: RuntimeLocal, C: Connector + Clone> {
    client: HttpEngine<R, C>,
    method: Method,
    uri: http::Uri,
    headers: HeaderMap,
    body: Option<crate::body::RequestBody>,
    timeout: Option<Duration>,
}

/// Newtype wrapper that implements [`HttpClient`] for the Local runtime path.
///
/// Needed because Rust's coherence rules prevent implementing `HttpClient`
/// for both `HttpEngine<R: RuntimePoll, ..>` and `HttpEngine<R: RuntimeLocal, ..>`.
pub struct HttpClientLocal<R: RuntimeLocal, C: Connector + Clone>(HttpEngine<R, C>);

impl<R: RuntimeLocal, C: Connector + Clone> Clone for HttpClientLocal<R, C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<R: RuntimeLocal, C: Connector + Clone> HttpClientLocal<R, C> {
    /// Wrap an [`HttpEngine`] to use via the [`HttpClient`] trait.
    pub fn new(engine: HttpEngine<R, C>) -> Self {
        Self(engine)
    }

    /// Get a reference to the inner engine.
    pub fn engine(&self) -> &HttpEngine<R, C> {
        &self.0
    }
}

impl<R: RuntimeLocal, C: Connector + Clone> From<HttpEngine<R, C>> for HttpClientLocal<R, C> {
    fn from(engine: HttpEngine<R, C>) -> Self {
        Self(engine)
    }
}

impl<R: RuntimeLocal, C: Connector + Clone> HttpClient for HttpClientLocal<R, C> {
    type RequestBuilder = OwnedRequestBuilderLocal<R, C>;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(OwnedRequestBuilderLocal {
            client: self.0.clone(),
            method,
            uri,
            headers: HeaderMap::new(),
            body: None,
            timeout: None,
        })
    }
}

impl<R: RuntimeLocal, C: Connector + Clone> RequestBuilderExt for OwnedRequestBuilderLocal<R, C> {
    type Response = Response<ResponseBoxLocalBody>;

    fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers.extend(headers);
        self
    }

    fn bearer_auth(mut self, token: &str) -> Self {
        let value = HeaderValue::from_str(&format!("Bearer {token}")).expect("valid bearer token");
        self.headers.insert(http::header::AUTHORIZATION, value);
        self
    }

    fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(crate::body::RequestBody::Buffered(body.into()));
        self
    }

    fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    async fn send(self) -> Result<Response<ResponseBoxLocalBody>, SendError> {
        let url = self.uri.clone();
        let effective_timeout = self.timeout.or(self.client.timeout);

        let execute_fut =
            self.client
                .execute_local(self.method, self.uri, self.headers, self.body, None);

        let result = match effective_timeout {
            Some(duration) => {
                crate::timeout::Timeout::WithTimeout {
                    future: execute_fut,
                    sleep: R::sleep(duration),
                }
                .await
            }
            None => execute_fut.await,
        };

        result.map_err(|error| SendError::new(error, url))
    }
}

impl ResponseExt for Response<ResponseBoxLocalBody> {
    fn status(&self) -> http::StatusCode {
        self.status()
    }

    fn headers(&self) -> &HeaderMap {
        self.headers()
    }

    async fn bytes(self) -> Result<Bytes, Error> {
        self.bytes().await
    }

    async fn text(self) -> Result<String, Error> {
        self.text().await
    }
}
