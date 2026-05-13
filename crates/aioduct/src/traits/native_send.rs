use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{HttpClient, RequestBuilderExt, ResponseExt};
use crate::client::HttpEngine;
use crate::error::{Error, SendError};
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll};

/// An owned request builder that does not borrow the [`HttpEngine`].
///
/// Returned by [`HttpClient::request()`] on [`HttpEngine`]. Internally wraps
/// a standard [`RequestBuilderSend`](crate::request::RequestBuilderSend) with an
/// owned client reference.
pub struct OwnedRequestBuilderSend<R: RuntimePoll, C: ConnectorSend> {
    inner: crate::request::RequestBuilderSend<'static, R, C>,
}

impl<R: RuntimePoll, C: ConnectorSend> HttpClient for HttpEngine<R, C> {
    type RequestBuilder = OwnedRequestBuilderSend<R, C>;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(OwnedRequestBuilderSend {
            inner: crate::request::RequestBuilderSend::new_owned(self.clone(), method, uri),
        })
    }
}

impl<R: RuntimePoll, C: ConnectorSend> RequestBuilderExt for OwnedRequestBuilderSend<R, C> {
    type Response = Response;

    fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.inner = self.inner.header(name, value);
        self
    }

    fn headers(mut self, headers: HeaderMap) -> Self {
        self.inner = self.inner.headers(headers);
        self
    }

    fn bearer_auth(mut self, token: &str) -> Self {
        self.inner = self.inner.bearer_auth(token);
        self
    }

    fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.inner = self.inner.body(body);
        self
    }

    fn timeout(mut self, duration: Duration) -> Self {
        self.inner = self.inner.timeout(duration);
        self
    }

    async fn send(self) -> Result<Response, SendError> {
        self.inner.send().await
    }
}

impl ResponseExt for Response {
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
