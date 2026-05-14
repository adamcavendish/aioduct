use std::marker::PhantomData;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode};

use crate::error::Error;
use crate::runtime::RuntimeCompletion;
use crate::traits::{HttpClient, RequestBuilderExt, ResponseExt};

/// A blocking HTTP client that wraps any async [`HttpClient`] implementor.
///
/// Uses [`RuntimeCompletion::block_on`] to execute async operations synchronously.
///
/// # Type aliases
///
/// For convenience, use the pre-configured type aliases:
/// - [`BlockingTokioClient`](crate::BlockingTokioClient)
/// - [`BlockingSmolClient`](crate::BlockingSmolClient)
/// - [`BlockingCompioClient`](crate::BlockingCompioClient)
pub struct BlockingClient<C: HttpClient, R: RuntimeCompletion> {
    inner: C,
    _runtime: PhantomData<R>,
}

impl<C: HttpClient, R: RuntimeCompletion> Clone for BlockingClient<C, R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _runtime: PhantomData,
        }
    }
}

impl<C: HttpClient, R: RuntimeCompletion> BlockingClient<C, R> {
    /// Create a blocking client wrapping the given async client.
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            _runtime: PhantomData,
        }
    }

    /// Get a reference to the inner async client.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    /// Start a GET request.
    pub fn get(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.get(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a HEAD request.
    pub fn head(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.head(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a POST request.
    pub fn post(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.post(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a PUT request.
    pub fn put(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.put(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a PATCH request.
    pub fn patch(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.patch(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a DELETE request.
    pub fn delete(&self, uri: &str) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.delete(uri)?,
            _runtime: PhantomData,
        })
    }

    /// Start a request with a custom method.
    pub fn request(
        &self,
        method: Method,
        uri: &str,
    ) -> Result<BlockingRequestBuilder<C, R>, Error> {
        Ok(BlockingRequestBuilder {
            inner: self.inner.request(method, uri)?,
            _runtime: PhantomData,
        })
    }
}

/// A blocking request builder.
pub struct BlockingRequestBuilder<C: HttpClient, R: RuntimeCompletion> {
    inner: C::RequestBuilder,
    _runtime: PhantomData<R>,
}

impl<C: HttpClient, R: RuntimeCompletion> BlockingRequestBuilder<C, R> {
    /// Add a typed header to the request.
    pub fn header(
        mut self,
        name: http::header::HeaderName,
        value: http::header::HeaderValue,
    ) -> Self {
        self.inner = self.inner.header(name, value);
        self
    }

    /// Add multiple headers to the request.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.inner = self.inner.headers(headers);
        self
    }

    /// Set a Bearer token Authorization header.
    pub fn bearer_auth(mut self, token: &str) -> Self {
        self.inner = self.inner.bearer_auth(token);
        self
    }

    /// Set a buffered request body.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.inner = self.inner.body(body);
        self
    }

    /// Set a timeout for this request.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.timeout(timeout);
        self
    }

    /// Send the request and block until the response is received.
    pub fn send(
        self,
    ) -> Result<BlockingResponse<<C::RequestBuilder as RequestBuilderExt>::Response, R>, Error>
    {
        let resp = R::block_on(self.inner.send())?.map_err(|e| e.into_error())?;
        Ok(BlockingResponse {
            inner: resp,
            _runtime: PhantomData,
        })
    }
}

/// A blocking HTTP response.
pub struct BlockingResponse<Resp: ResponseExt, R: RuntimeCompletion> {
    inner: Resp,
    _runtime: PhantomData<R>,
}

impl<Resp: ResponseExt, R: RuntimeCompletion> BlockingResponse<Resp, R> {
    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// Returns the response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    /// Returns the Content-Length header value, if present.
    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .headers()
            .get(http::header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }

    /// Returns an error if the status is 4xx or 5xx, consuming the response.
    pub fn error_for_status(self) -> Result<Self, Error> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Returns an error if the status is 4xx or 5xx, without consuming the response.
    pub fn error_for_status_ref(&self) -> Result<&Self, Error> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Consume the response body and return it as bytes.
    pub fn bytes(self) -> Result<Bytes, Error> {
        R::block_on(self.inner.bytes())?
    }

    /// Consume the response body and return it as a UTF-8 string.
    pub fn text(self) -> Result<String, Error> {
        R::block_on(self.inner.text())?
    }
}

impl<Resp: ResponseExt, R: RuntimeCompletion> std::fmt::Debug for BlockingResponse<Resp, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockingResponse")
            .field("status", &self.inner.status())
            .finish_non_exhaustive()
    }
}

/// Additional methods available when the inner response is [`Response`](crate::Response).
#[cfg(not(target_arch = "wasm32"))]
impl<R: RuntimeCompletion> BlockingResponse<crate::Response, R> {
    /// Returns the final URL of this response, after any redirects.
    pub fn url(&self) -> &http::Uri {
        self.inner.url()
    }

    /// Returns the remote socket address of the server.
    pub fn remote_addr(&self) -> Option<std::net::SocketAddr> {
        self.inner.remote_addr()
    }

    /// Returns the HTTP version.
    pub fn version(&self) -> http::Version {
        self.inner.version()
    }

    /// Returns TLS handshake info, if the connection used TLS.
    pub fn tls_info(&self) -> Option<&crate::tls::TlsInfo> {
        self.inner.tls_info()
    }

    /// Consume the response body and deserialize it as JSON.
    #[cfg(feature = "json")]
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        R::block_on(self.inner.json())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_tokio_client_builds() {
        use crate::runtime::tokio_rt::TcpConnector;
        let engine =
            crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::new(TcpConnector);
        let _client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn blocking_tokio_default_headers() {
        use crate::runtime::tokio_rt::TcpConnector;
        let engine = crate::HttpEngineSend::<crate::runtime::TokioRuntime, TcpConnector>::builder(
            TcpConnector,
        )
        .user_agent("blocking-test/1.0")
        .build();
        let client = BlockingClient::<_, crate::runtime::TokioRuntime>::new(engine);
        let result = client.get("http://127.0.0.1:1/nonexistent");
        assert!(result.is_ok());
    }
}
