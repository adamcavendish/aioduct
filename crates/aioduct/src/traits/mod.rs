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
    fn connect_timeout(self, duration: Duration) -> Self;

    /// Set a per-request read timeout for gaps between response body chunks.
    fn read_timeout(self, duration: Duration) -> Self;

    /// Disable automatic response decompression for this request.
    fn no_decompression(self) -> Self;

    /// Set HTTP Basic auth credentials.
    fn basic_auth(self, username: &str, password: Option<&str>) -> Self;

    /// Append query parameters to the URL.
    fn query(self, params: &[(&str, &str)]) -> Self;

    /// Set a URL-encoded form body from string pairs.
    fn form(self, params: &[(&str, &str)]) -> Self;

    /// Force a specific HTTP version.
    fn version(self, version: http::Version) -> Self;

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
    /// Future returned by [`next()`](ByteStreamExt::next).
    ///
    /// Generic callers that need a `Send` reading task can additionally require
    /// `Self: Send` and `for<'a> Self::Next<'a>: Send`. Local streams need not
    /// satisfy these bounds. The latter bound can require `'static` for streams
    /// borrowing external data due to current compiler limitations.
    type Next<'a>: Future<Output = Option<Result<Bytes, Error>>>
    where
        Self: 'a;

    /// Returns the next chunk of body data, or `None` when complete.
    fn next(&mut self) -> Self::Next<'_>;
}

/// Response capability for establishing an associated SSE stream.
///
/// Separate from `ResponseExt`: responses without SSE support need not invent
/// a runtime unsupported fallback. Returned types do not carry Observer types.
pub trait SseResponseExt: ResponseExt + Sized {
    /// The associated SSE stream, including its private continuation.
    type SseStream: SseStreamExt;
    /// Establish the SSE path; no body read is required yet.
    fn into_sse_stream(self) -> Self::SseStream;
}

/// SSE stream capability for generic async consumers, including Local streams.
pub trait SseStreamExt: Sized {
    /// Named future returned by `next`. Send callers may additionally require
    /// `Self: Send` and `for<'a> Self::Next<'a>: Send`, as with `ByteStreamExt`.
    /// This HRTB can imply `'static` for borrowed third-party streams on current Rust.
    type Next<'a>: Future<Output = Option<Result<crate::sse::SseEvent, Error>>>
    where
        Self: 'a;
    /// Return the next protocol event, or None when the SSE stream has ended.
    fn next(&mut self) -> Self::Next<'_>;
    /// Set the local event payload limit; zero disables it.
    fn with_max_payload_size(self, max: usize) -> Self;
}

impl<B: http_body::Body<Data = Bytes, Error = Error> + Unpin> SseStreamExt
    for crate::sse::SseStream<B>
{
    type Next<'a>
        = crate::sse::SseNext<'a, B>
    where
        Self: 'a;
    fn next(&mut self) -> Self::Next<'_> {
        self.next()
    }

    fn with_max_payload_size(self, max: usize) -> Self {
        self.with_max_payload_size(max)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SseStreamExt for crate::sse::SseStreamLocal {
    type Next<'a> = crate::sse::SseNextLocal<'a>;
    fn next(&mut self) -> Self::Next<'_> {
        self.next()
    }

    fn with_max_payload_size(self, max: usize) -> Self {
        self.with_max_payload_size(max)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SseResponseExt for crate::response::Response {
    type SseStream = crate::sse::SseStreamSend;
    fn into_sse_stream(self) -> Self::SseStream {
        self.into_sse_stream()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SseResponseExt for crate::response::Response<crate::body::ResponseBodyLocal> {
    type SseStream = crate::sse::SseStreamLocal;
    fn into_sse_stream(self) -> Self::SseStream {
        self.into_sse_stream()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native_local::OwnedRequestBuilderLocal;
#[cfg(not(target_arch = "wasm32"))]
pub use native_send::OwnedRequestBuilderSend;
#[cfg(feature = "wasi-p2")]
pub use wasi::OwnedWasiRequestBuilder;
#[cfg(feature = "wasm")]
pub use wasm::OwnedWasmRequestBuilder;
