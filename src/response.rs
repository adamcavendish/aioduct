use std::net::SocketAddr;

use bytes::Bytes;
use http::header::{CONTENT_LENGTH, HeaderMap};
use http::{StatusCode, Uri, Version};
use http_body_util::BodyExt;

use crate::error::{Error, HyperBody, Result};

/// An HTTP response with status, headers, and a streaming body.
pub struct Response {
    inner: http::Response<HyperBody>,
    url: Uri,
    remote_addr: Option<SocketAddr>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.inner.status())
            .field("version", &self.inner.version())
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Response {
    pub(crate) fn new(inner: http::Response<HyperBody>, url: Uri) -> Self {
        Self {
            inner,
            url,
            remote_addr: None,
        }
    }

    pub(crate) fn set_remote_addr(&mut self, addr: Option<SocketAddr>) {
        self.remote_addr = addr;
    }

    pub(crate) fn inner_mut(&mut self) -> &mut http::Response<HyperBody> {
        &mut self.inner
    }

    pub(crate) fn decompress(self, accept: &crate::decompress::AcceptEncoding) -> Self {
        let (mut parts, body) = self.inner.into_parts();
        let body = crate::decompress::maybe_decompress(&mut parts.headers, body, accept);
        Self {
            inner: http::Response::from_parts(parts, body),
            url: self.url,
            remote_addr: self.remote_addr,
        }
    }

    pub(crate) fn apply_read_timeout<R: crate::runtime::Runtime>(
        self,
        duration: std::time::Duration,
    ) -> Self {
        use http_body_util::BodyExt;
        let (parts, body) = self.inner.into_parts();
        let timeout_body = crate::timeout::ReadTimeoutBody::<R>::new(body, duration);
        let boxed: HyperBody = timeout_body.map_err(|e| e).boxed();
        Self {
            inner: http::Response::from_parts(parts, boxed),
            url: self.url,
            remote_addr: self.remote_addr,
        }
    }

    /// Returns the final URL of this response, after any redirects.
    pub fn url(&self) -> &Uri {
        &self.url
    }

    /// Returns the remote socket address of the server.
    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.remote_addr
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// Returns the response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    /// Returns a mutable reference to the response headers.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.inner.headers_mut()
    }

    /// Returns a reference to the response extensions.
    pub fn extensions(&self) -> &http::Extensions {
        self.inner.extensions()
    }

    /// Returns a mutable reference to the response extensions.
    pub fn extensions_mut(&mut self) -> &mut http::Extensions {
        self.inner.extensions_mut()
    }

    /// Returns the HTTP version.
    pub fn version(&self) -> Version {
        self.inner.version()
    }

    /// Returns an error if the response status is a client (4xx) or server (5xx) error.
    pub fn error_for_status(self) -> Result<Self> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Returns an error reference if the status is 4xx or 5xx, without consuming the response.
    pub fn error_for_status_ref(&self) -> Result<&Self> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Returns the Content-Length header value, if present.
    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .headers()
            .get(CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }

    /// Consume the response body and return it as bytes.
    pub async fn bytes(self) -> Result<Bytes> {
        let body = self.inner.into_body();
        let collected = body
            .collect()
            .await
            .map_err(|e| Error::Other(Box::new(e)))?;
        Ok(collected.to_bytes())
    }

    /// Consume the response body and return it as a UTF-8 string.
    pub async fn text(self) -> Result<String> {
        let bytes = self.bytes().await?;
        String::from_utf8(bytes.to_vec()).map_err(|e| Error::Other(Box::new(e)))
    }

    /// Consume the response body and deserialize it as JSON.
    #[cfg(feature = "json")]
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Other(Box::new(e)))
    }

    /// Consume the response and return the raw hyper body.
    pub fn into_body(self) -> HyperBody {
        self.inner.into_body()
    }

    /// Convert the response into an async byte stream.
    pub fn into_bytes_stream(self) -> crate::body::BodyStream {
        crate::body::BodyStream::new(self.inner.into_body())
    }

    /// Convert the response into a Server-Sent Events stream.
    pub fn into_sse_stream(self) -> crate::sse::SseStream {
        crate::sse::SseStream::new(self.inner.into_body())
    }

    /// Perform an HTTP upgrade (e.g., WebSocket) on this response.
    ///
    /// This should be called after receiving a `101 Switching Protocols` response.
    /// Returns an [`Upgraded`](crate::upgrade::Upgraded) bidirectional IO stream.
    pub async fn upgrade(mut self) -> Result<crate::upgrade::Upgraded> {
        crate::upgrade::on_upgrade(&mut self.inner).await
    }
}
