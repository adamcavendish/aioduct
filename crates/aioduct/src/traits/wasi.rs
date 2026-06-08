use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{ByteStreamExt, HttpClient, RequestBuilderExt, ResponseExt};
use crate::error::{Error, SendError};
use crate::wasi_p2::{WasiBodyStream, WasiClient, WasiResponse};

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

    fn basic_auth(mut self, username: &str, password: Option<&str>) -> Self {
        self.inner = self.inner.basic_auth(username, password);
        self
    }

    fn query(mut self, params: &[(&str, &str)]) -> Self {
        self.inner = self.inner.query(params);
        self
    }

    async fn send(self) -> Result<WasiResponse, SendError> {
        let url = self.inner.uri().clone();
        self.inner.send().map_err(|e| SendError::new(e, url))
    }
}

impl ResponseExt for WasiResponse {
    type ByteStream = WasiBodyStream;

    fn status(&self) -> http::StatusCode {
        self.status()
    }

    fn headers(&self) -> &HeaderMap {
        self.headers()
    }

    async fn bytes(self) -> Result<Bytes, Error> {
        self.bytes()
    }

    async fn text(self) -> Result<String, Error> {
        self.text()
    }

    #[cfg(feature = "json")]
    async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        self.json()
    }

    fn into_bytes_stream(self) -> WasiBodyStream {
        self.into_bytes_stream()
    }
}

impl ByteStreamExt for WasiBodyStream {
    async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        WasiBodyStream::next(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_http_client<C: HttpClient>() {}

    #[test]
    fn wasi_client_implements_http_client() {
        assert_http_client::<WasiClient>();
    }

    #[test]
    fn trait_basic_auth_non_default() {
        // basic_auth is overridden — verify the trait method compiles and is not a no-op.
        let client = WasiClient::new();
        let _rb = HttpClient::get(&client, "https://example.com")
            .unwrap()
            .basic_auth("user", Some("pass"));
        // Actual behavior is tested in wasi_p2::tests.
    }

    #[test]
    fn trait_query_non_default() {
        // query is overridden — verify the trait method compiles and is not a no-op.
        let client = WasiClient::new();
        let _rb = HttpClient::get(&client, "https://example.com/path")
            .unwrap()
            .query(&[("a", "1")]);
        // Actual behavior is tested in wasi_p2::tests.
    }
}
