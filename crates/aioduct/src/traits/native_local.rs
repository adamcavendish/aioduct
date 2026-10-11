use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{ByteStreamExt, HttpClient, RequestBuilderExt, ResponseExt};
use crate::body::{BodyStreamLocal, ResponseBodyLocal};
use crate::client::HttpEngineLocal;
use crate::error::{Error, SendError};
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal};

/// An owned request builder that does not borrow the [`HttpEngineLocal`].
///
/// Returned by [`HttpClient::request()`] on [`HttpEngineLocal`]. Internally wraps
/// a standard [`RequestBuilderLocal`](crate::request::RequestBuilderLocal) with an
/// owned client reference.
pub struct OwnedRequestBuilderLocal<
    R: RuntimeLocal,
    C: ConnectorLocal + Clone,
    O: crate::observer::Observer = crate::observer::NoopObserver,
> {
    inner: crate::request::RequestBuilderLocal<'static, R, C, O>,
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone, O: crate::observer::Observer>
    OwnedRequestBuilderLocal<R, C, O>
{
    /// Skip observation for this logical request, before `Observer::begin`.
    pub fn disable_observation(mut self) -> Self {
        self.inner = self.inner.disable_observation();
        self
    }

    /// Supply request-scoped context to the configured observer.
    pub fn observation_context(mut self, context: O::Context) -> Self {
        self.inner = self.inner.observation_context(context);
        self
    }
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone, O: crate::observer::Observer> HttpClient
    for HttpEngineLocal<R, C, O>
{
    type RequestBuilder = OwnedRequestBuilderLocal<R, C, O>;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let (uri, fragment) =
            crate::client::resolve_request_url(self.core.base_url.as_deref(), uri)?;
        Ok(OwnedRequestBuilderLocal {
            inner: crate::request::RequestBuilderLocal::new_owned(
                self.clone(),
                method,
                uri,
                fragment,
            ),
        })
    }
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone, O: crate::observer::Observer> RequestBuilderExt
    for OwnedRequestBuilderLocal<R, C, O>
{
    type Response = Response<ResponseBodyLocal>;

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

    async fn send(self) -> Result<Response<ResponseBodyLocal>, SendError> {
        let url = self.inner.uri().clone();
        self.inner.send().await.map_err(|e| SendError::new(e, url))
    }
}

impl ResponseExt for Response<ResponseBodyLocal> {
    type ByteStream = BodyStreamLocal;

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

    fn into_bytes_stream(self) -> BodyStreamLocal {
        self.into_bytes_stream()
    }
}

impl ByteStreamExt for BodyStreamLocal {
    type Next<'a> = crate::body::BodyNextLocal<'a>;

    fn next(&mut self) -> Self::Next<'_> {
        BodyStreamLocal::next(self)
    }
}
