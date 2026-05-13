use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{HttpClient, RequestBuilderExt, ResponseExt};
use crate::error::{Error, SendError};
use crate::wasi_p2::{WasiClient, WasiResponse};

/// An owned request builder for the WASI client.
pub struct OwnedWasiRequestBuilder {
    inner: crate::wasi_p2::WasiRequestBuilder<'static>,
}

impl HttpClient for WasiClient {
    type RequestBuilder = OwnedWasiRequestBuilder;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(OwnedWasiRequestBuilder {
            inner: crate::wasi_p2::WasiRequestBuilder::new_owned(self.clone(), method, uri),
        })
    }
}

impl RequestBuilderExt for OwnedWasiRequestBuilder {
    type Response = WasiResponse;

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

    async fn send(self) -> Result<WasiResponse, SendError> {
        let url = self.inner.uri().clone();
        self.inner.send().map_err(|e| SendError::new(e, url))
    }
}

impl ResponseExt for WasiResponse {
    fn status(&self) -> http::StatusCode {
        self.status()
    }

    fn headers(&self) -> &HeaderMap {
        self.headers()
    }

    async fn bytes(self) -> Result<Bytes, Error> {
        Ok(self.bytes())
    }

    async fn text(self) -> Result<String, Error> {
        self.text()
    }
}
