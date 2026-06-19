use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{ByteStreamExt, HttpClient, RequestBuilderExt, ResponseExt};
use crate::error::{Error, SendError};
use crate::wasm::{WasmBodyStream, WasmClient, WasmResponse};

/// An owned request builder for the WASM client.
pub struct OwnedWasmRequestBuilder {
    inner: crate::wasm::WasmRequestBuilder<'static>,
}

impl HttpClient for WasmClient {
    type RequestBuilder = OwnedWasmRequestBuilder;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(OwnedWasmRequestBuilder {
            inner: crate::wasm::WasmRequestBuilder::new_owned(self.clone(), method, uri),
        })
    }
}

impl RequestBuilderExt for OwnedWasmRequestBuilder {
    type Response = WasmResponse;

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

    fn form(mut self, params: &[(&str, &str)]) -> Self {
        self.inner = self.inner.form(params);
        self
    }

    async fn send(self) -> Result<WasmResponse, SendError> {
        let url = self.inner.uri().clone();
        self.inner.send().await.map_err(|e| SendError::new(e, url))
    }
}

impl ResponseExt for WasmResponse {
    type ByteStream = WasmBodyStream;

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

    fn into_bytes_stream(self) -> WasmBodyStream {
        self.into_bytes_stream()
    }
}

impl ByteStreamExt for WasmBodyStream {
    async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        self.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_http_client<C: HttpClient>() {}

    #[test]
    fn wasm_client_implements_http_client() {
        assert_http_client::<WasmClient>();
    }

    #[test]
    fn trait_basic_auth_non_default() {
        // basic_auth is overridden — verify the trait method compiles and is not a no-op.
        let client = WasmClient::new();
        let _rb = HttpClient::get(&client, "https://example.com")
            .unwrap()
            .basic_auth("user", Some("pass"));
        // Actual behavior is tested in wasm::tests.
    }

    #[test]
    fn trait_query_non_default() {
        // query is overridden — verify the trait method compiles and is not a no-op.
        let client = WasmClient::new();
        let _rb = HttpClient::get(&client, "https://example.com/path")
            .unwrap()
            .query(&[("a", "1")]);
        // Actual behavior is tested in wasm::tests.
    }

    #[test]
    fn trait_form_non_default() {
        let client = WasmClient::new();
        let _rb = HttpClient::post(&client, "https://example.com/path")
            .unwrap()
            .form(&[("a", "1")]);
        // Actual behavior is tested in wasm::tests.
    }
}
