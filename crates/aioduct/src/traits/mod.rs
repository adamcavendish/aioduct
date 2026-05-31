//! Consumer-facing client trait and extension traits.
//!
//! These traits provide a unified interface across `HttpEngineSend`,
//! `WasmClient`, and `WasiClient`.

#[cfg(not(target_arch = "wasm32"))]
mod native_local;
#[cfg(not(target_arch = "wasm32"))]
mod native_send;
#[cfg(all(test, not(target_arch = "wasm32"), feature = "tokio"))]
mod tests;
#[cfg(feature = "wasi-p2")]
mod wasi;
#[cfg(feature = "wasm")]
mod wasm;

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

    /// Set a per-request connect timeout.
    ///
    /// Implementations that cannot control connection establishment may ignore
    /// this option.
    fn connect_timeout(self, _duration: Duration) -> Self {
        self
    }

    /// Send the request and return the response.
    fn send(self) -> impl Future<Output = Result<Self::Response, SendError>>;
}

/// Common interface for reading HTTP responses.
pub trait ResponseExt {
    /// The byte stream type returned by [`into_bytes_stream()`](ResponseExt::into_bytes_stream).
    type ByteStream: ByteStreamExt;

    /// Returns the HTTP status code.
    fn status(&self) -> StatusCode;

    /// Returns the response headers.
    fn headers(&self) -> &HeaderMap;

    /// Consume the response and return the body as bytes.
    fn bytes(self) -> impl Future<Output = Result<Bytes, Error>>;

    /// Consume the response and return the body as a UTF-8 string.
    fn text(self) -> impl Future<Output = Result<String, Error>>;

    /// Consume the response and deserialize the body as JSON.
    #[cfg(feature = "json")]
    fn json<T: serde::de::DeserializeOwned>(self) -> impl Future<Output = Result<T, Error>>;

    /// Convert the response into an async byte stream.
    fn into_bytes_stream(self) -> Self::ByteStream;
}

/// Async byte stream that yields chunks of body data.
pub trait ByteStreamExt {
    /// Returns the next chunk of body data, or `None` when complete.
    fn next(&mut self) -> impl Future<Output = Option<Result<Bytes, Error>>>;
}

#[cfg(not(target_arch = "wasm32"))]
pub use native_local::OwnedRequestBuilderLocal;
#[cfg(not(target_arch = "wasm32"))]
pub use native_send::OwnedRequestBuilderSend;
#[cfg(feature = "wasi-p2")]
pub use wasi::OwnedWasiRequestBuilder;
#[cfg(feature = "wasm")]
pub use wasm::OwnedWasmRequestBuilder;
