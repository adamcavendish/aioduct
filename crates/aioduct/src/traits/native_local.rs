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
pub struct OwnedRequestBuilderLocal<R: RuntimeLocal, C: ConnectorLocal + Clone> {
    inner: crate::request::RequestBuilderLocal<'static, R, C>,
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> HttpClient for HttpEngineLocal<R, C> {
    type RequestBuilder = OwnedRequestBuilderLocal<R, C>;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(OwnedRequestBuilderLocal {
            inner: crate::request::RequestBuilderLocal::new_owned(self.clone(), method, uri),
        })
    }
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> RequestBuilderExt
    for OwnedRequestBuilderLocal<R, C>
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
    async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        self.next().await
    }
}
