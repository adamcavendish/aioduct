use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{ByteStreamExt, HttpClient, RequestBuilderExt, ResponseExt};
use crate::body::BodyStreamSend;
use crate::client::HttpEngineSend;
use crate::error::{Error, SendError};
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll};

/// An owned request builder that does not borrow the [`HttpEngineSend`].
///
/// Returned by [`HttpClient::request()`] on [`HttpEngineSend`]. Internally wraps
/// a standard [`RequestBuilderSend`](crate::request::RequestBuilderSend) with an
/// owned client reference.
pub struct OwnedRequestBuilderSend<R: RuntimePoll, C: ConnectorSend> {
    inner: crate::request::RequestBuilderSend<'static, R, C>,
}

impl<R: RuntimePoll, C: ConnectorSend> HttpClient for HttpEngineSend<R, C> {
    type RequestBuilder = OwnedRequestBuilderSend<R, C>;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let (uri, fragment) =
            crate::client::resolve_request_url(self.core.base_url.as_deref(), uri)?;
        Ok(OwnedRequestBuilderSend {
            inner: crate::request::RequestBuilderSend::new_owned(
                self.clone(),
                method,
                uri,
                fragment,
            ),
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

    fn connect_timeout(mut self, duration: Duration) -> Self {
        self.inner = self.inner.connect_timeout(duration);
        self
    }

    fn read_timeout(mut self, duration: Duration) -> Self {
        self.inner = self.inner.read_timeout(duration);
        self
    }

    fn no_decompression(mut self) -> Self {
        self.inner = self.inner.no_decompression();
        self
    }

    fn basic_auth(mut self, username: &str, password: Option<&str>) -> Self {
        self.inner = self.inner.basic_auth(username, password);
        self
    }

    fn query(mut self, params: &[(&str, &str)]) -> Self {
        self.inner = self.inner.query(params);
        self
    }

    fn form(mut self, params: &[(&str, &str)]) -> Self {
        self.inner = self.inner.form(params);
        self
    }

    fn version(mut self, version: http::Version) -> Self {
        self.inner = self.inner.version(version);
        self
    }

    async fn send(self) -> Result<Response, SendError> {
        self.inner.send().await
    }
}

impl ResponseExt for Response {
    type ByteStream = BodyStreamSend;

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

    #[cfg(feature = "json")]
    async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        self.json().await
    }

    fn into_bytes_stream(self) -> BodyStreamSend {
        self.into_bytes_stream()
    }
}

impl ByteStreamExt for BodyStreamSend {
    async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        self.next().await
    }
}
