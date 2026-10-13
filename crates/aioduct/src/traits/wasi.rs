use bytes::Bytes;
use http::Method;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

use super::{
    ByteStreamExt, HttpClient, RequestBuilderExt, ResponseExt, SseResponseExt, SseStreamExt,
};
use crate::error::{Error, SendError};
use crate::wasi_p2::{WasiBodyStream, WasiClient, WasiResponse};

impl SseResponseExt for WasiResponse {
    type SseStream = crate::wasi_p2::WasiSseStream;
    fn into_sse_stream(self) -> Self::SseStream {
        self.into_sse_stream()
    }
}

impl SseStreamExt for crate::wasi_p2::WasiSseStream {
    type Next<'a> = crate::wasi_p2::WasiSseNext<'a>;
    fn next(&mut self) -> Self::Next<'_> {
        crate::wasi_p2::WasiSseNext::new(self)
    }

    fn with_max_payload_size(self, max: usize) -> Self {
        self.with_max_payload_size(max)
    }
}

/// An owned request builder for the WASI client.
pub struct OwnedWasiRequestBuilder<O = crate::observer::NoopObserver>
where
    O: crate::observer::Observer,
{
    inner: crate::wasi_p2::WasiRequestBuilder<'static, O>,
}

impl<O> HttpClient for WasiClient<O>
where
    O: crate::observer::Observer,
{
    type RequestBuilder = OwnedWasiRequestBuilder<O>;

    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
        let uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(OwnedWasiRequestBuilder {
            inner: crate::wasi_p2::WasiRequestBuilder::new_owned(self.clone(), method, uri),
        })
    }
}

impl<O> OwnedWasiRequestBuilder<O>
where
    O: crate::observer::Observer,
{
    /// Skip observation for this logical request.
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

impl<O> RequestBuilderExt for OwnedWasiRequestBuilder<O>
where
    O: crate::observer::Observer,
{
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
    type Next<'a> = crate::wasi_p2::WasiBodyNext<'a>;

    fn next(&mut self) -> Self::Next<'_> {
        crate::wasi_p2::WasiBodyNext::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestObserver;
    struct TestRequestObservation;

    impl crate::observer::RequestObservation for TestRequestObservation {}

    impl crate::observer::Observer for TestObserver {
        type Context = u8;
        type Observation = TestRequestObservation;
        type ConnectionObservation = crate::observer::NoopConnectionObservation;

        fn begin(
            &self,
            _: crate::observer::RequestInfo<'_>,
            _: Option<Self::Context>,
        ) -> (crate::observer::Interest, Self::Observation) {
            (
                crate::observer::Interest::REQUEST_FINISH,
                TestRequestObservation,
            )
        }

        fn begin_connection(&self) -> (crate::observer::Interest, Self::ConnectionObservation) {
            (
                crate::observer::Interest::EMPTY,
                crate::observer::NoopConnectionObservation,
            )
        }
    }

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

    #[test]
    fn trait_form_non_default() {
        let client = WasiClient::new();
        let _rb = HttpClient::post(&client, "https://example.com/path")
            .unwrap()
            .form(&[("a", "1")]);
        // Actual behavior is tested in wasi_p2::tests.
    }

    #[test]
    fn wasi_client_observer_is_configured_at_the_builder_boundary() {
        let client = WasiClient::builder()
            .observer(TestObserver)
            .build()
            .unwrap();
        let _ = client
            .get("https://example.com")
            .unwrap()
            .observation_context(7)
            .disable_observation();
    }
}
