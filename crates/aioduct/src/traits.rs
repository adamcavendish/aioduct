//! Consumer-facing client trait and extension traits.
//!
//! These traits provide a unified interface across [`HttpEngine`](crate::HttpEngine),
//! [`WasmClient`](crate::wasm::WasmClient), and [`WasiClient`](crate::wasi_p2::WasiClient).

use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http::{Method, StatusCode};

use crate::error::{Error, SendError};

/// Unified HTTP client trait — consumers program against this.
///
/// Each implementor provides its own `RequestBuilder` and `Response` types.
/// Convenience methods (`get`, `post`, etc.) are provided as default methods.
pub trait HttpClient: Clone + 'static {
    /// The request builder type returned by [`request()`](HttpClient::request).
    type RequestBuilder: RequestBuilderExt;

    /// Start a request with the given HTTP method and URL.
    fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error>;

    /// Start a GET request.
    fn get(&self, uri: &str) -> Result<Self::RequestBuilder, Error> {
        self.request(Method::GET, uri)
    }

    /// Start a HEAD request.
    fn head(&self, uri: &str) -> Result<Self::RequestBuilder, Error> {
        self.request(Method::HEAD, uri)
    }

    /// Start a POST request.
    fn post(&self, uri: &str) -> Result<Self::RequestBuilder, Error> {
        self.request(Method::POST, uri)
    }

    /// Start a PUT request.
    fn put(&self, uri: &str) -> Result<Self::RequestBuilder, Error> {
        self.request(Method::PUT, uri)
    }

    /// Start a PATCH request.
    fn patch(&self, uri: &str) -> Result<Self::RequestBuilder, Error> {
        self.request(Method::PATCH, uri)
    }

    /// Start a DELETE request.
    fn delete(&self, uri: &str) -> Result<Self::RequestBuilder, Error> {
        self.request(Method::DELETE, uri)
    }
}

/// Common interface for building and sending HTTP requests.
///
/// This trait is **not** object-safe due to `impl Future` return types and
/// `impl Into<Bytes>` parameters. Use concrete types or generics (`C: HttpClient`)
/// rather than `dyn RequestBuilderExt`.
pub trait RequestBuilderExt: Sized {
    /// The response type returned by [`send()`](RequestBuilderExt::send).
    type Response: ResponseExt;

    /// Add a header to the request.
    fn header(self, name: HeaderName, value: HeaderValue) -> Self;

    /// Add multiple headers to the request.
    fn headers(self, headers: HeaderMap) -> Self;

    /// Set a Bearer token Authorization header.
    fn bearer_auth(self, token: &str) -> Self;

    /// Set a buffered request body.
    fn body(self, body: impl Into<Bytes>) -> Self;

    /// Set a per-request timeout.
    fn timeout(self, duration: Duration) -> Self;

    /// Send the request and return the response.
    fn send(self) -> impl Future<Output = Result<Self::Response, SendError>>;
}

/// Common interface for reading HTTP responses.
pub trait ResponseExt {
    /// Returns the HTTP status code.
    fn status(&self) -> StatusCode;

    /// Returns the response headers.
    fn headers(&self) -> &HeaderMap;

    /// Consume the response and return the body as bytes.
    fn bytes(self) -> impl Future<Output = Result<Bytes, Error>>;

    /// Consume the response and return the body as a UTF-8 string.
    fn text(self) -> impl Future<Output = Result<String, Error>>;
}

// ── Native implementations ─────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod native_impls {
    use super::*;
    use crate::client::HttpEngine;
    use crate::response::Response;
    use crate::runtime::{ConnectorSend, RuntimePoll};

    /// An owned request builder that does not borrow the [`HttpEngine`].
    ///
    /// Returned by [`HttpClient::request()`] on [`HttpEngine`]. Internally wraps
    /// a standard [`RequestBuilder`](crate::request::RequestBuilder) with an
    /// owned client reference.
    pub struct OwnedRequestBuilder<R: RuntimePoll, C: ConnectorSend> {
        inner: crate::request::RequestBuilder<'static, R, C>,
    }

    impl<R: RuntimePoll, C: ConnectorSend> HttpClient for HttpEngine<R, C> {
        type RequestBuilder = OwnedRequestBuilder<R, C>;

        fn request(&self, method: Method, uri: &str) -> Result<Self::RequestBuilder, Error> {
            let uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
            Ok(OwnedRequestBuilder {
                inner: crate::request::RequestBuilder::new_owned(self.clone(), method, uri),
            })
        }
    }

    impl<R: RuntimePoll, C: ConnectorSend> RequestBuilderExt for OwnedRequestBuilder<R, C> {
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
        fn status(&self) -> StatusCode {
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
}

#[cfg(not(target_arch = "wasm32"))]
pub use native_impls::OwnedRequestBuilder;

// ── WASM implementations ──────────────────────────────────────────────────

#[cfg(feature = "wasm")]
mod wasm_impls {
    use super::*;
    use crate::wasm::{WasmClient, WasmResponse};

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

        async fn send(self) -> Result<WasmResponse, SendError> {
            let url = self.inner.uri().clone();
            self.inner.send().await.map_err(|e| SendError::new(e, url))
        }
    }

    impl ResponseExt for WasmResponse {
        fn status(&self) -> StatusCode {
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
}

#[cfg(feature = "wasm")]
pub use wasm_impls::OwnedWasmRequestBuilder;

// ── WASI implementations ──────────────────────────────────────────────────

#[cfg(feature = "wasi-p2")]
mod wasi_impls {
    use super::*;
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
        fn status(&self) -> StatusCode {
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
}

#[cfg(feature = "wasi-p2")]
pub use wasi_impls::OwnedWasiRequestBuilder;

#[cfg(all(test, not(target_arch = "wasm32"), feature = "tokio"))]
mod tests {
    use super::*;
    use crate::client::HttpEngine;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};

    fn assert_http_client<C: HttpClient>() {}

    #[test]
    fn http_engine_implements_http_client() {
        assert_http_client::<HttpEngine<TokioRuntime, TcpConnector>>();
    }

    fn generic_build<C: HttpClient>(client: &C) -> Result<C::RequestBuilder, Error> {
        client
            .get("http://example.com")?
            .header(
                http::header::ACCEPT,
                http::header::HeaderValue::from_static("text/html"),
            )
            .body("test")
            .timeout(std::time::Duration::from_secs(5));
        client.post("http://example.com")
    }

    #[test]
    fn generic_request_building() {
        let engine = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
        let builder = generic_build(&engine);
        assert!(builder.is_ok());
    }
}
