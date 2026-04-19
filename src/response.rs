use std::net::SocketAddr;

use bytes::Bytes;
use http::header::{CONTENT_LENGTH, HeaderMap};
use http::{StatusCode, Uri, Version};
use http_body_util::BodyExt;

use crate::error::{Error, HyperBody};

/// An HTTP response with status, headers, and a streaming body.
pub struct Response {
    inner: http::Response<HyperBody>,
    url: Uri,
    remote_addr: Option<SocketAddr>,
    tls_info: Option<crate::tls::TlsInfo>,
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
            tls_info: None,
        }
    }

    pub(crate) fn set_remote_addr(&mut self, addr: Option<SocketAddr>) {
        self.remote_addr = addr;
    }

    pub(crate) fn set_tls_info(&mut self, info: Option<crate::tls::TlsInfo>) {
        self.tls_info = info;
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
            tls_info: self.tls_info,
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
            tls_info: self.tls_info,
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

    /// Returns TLS handshake info (peer certificate), if the connection used TLS.
    pub fn tls_info(&self) -> Option<&crate::tls::TlsInfo> {
        self.tls_info.as_ref()
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
    pub fn error_for_status(self) -> Result<Self, Error> {
        let status = self.inner.status();
        if status.is_client_error() || status.is_server_error() {
            Err(Error::Status(status))
        } else {
            Ok(self)
        }
    }

    /// Returns an error reference if the status is 4xx or 5xx, without consuming the response.
    pub fn error_for_status_ref(&self) -> Result<&Self, Error> {
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
    pub async fn bytes(self) -> Result<Bytes, Error> {
        let body = self.inner.into_body();
        let collected = body
            .collect()
            .await
            .map_err(|e| Error::Other(Box::new(e)))?;
        Ok(collected.to_bytes())
    }

    /// Consume the response body and return it as a UTF-8 string.
    pub async fn text(self) -> Result<String, Error> {
        #[cfg(feature = "charset")]
        {
            self.text_with_charset("utf-8").await
        }
        #[cfg(not(feature = "charset"))]
        {
            let bytes = self.bytes().await?;
            String::from_utf8(bytes.to_vec()).map_err(|e| Error::Other(Box::new(e)))
        }
    }

    #[cfg(feature = "charset")]
    /// Consume the response body and decode it using the charset from Content-Type,
    /// falling back to the given default encoding.
    pub async fn text_with_charset(self, default_encoding: &str) -> Result<String, Error> {
        let content_type = self
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<mime::Mime>().ok());
        let encoding_name = content_type
            .as_ref()
            .and_then(|mime| mime.get_param("charset"))
            .map(|charset| charset.as_str())
            .unwrap_or(default_encoding);
        let encoding = encoding_rs::Encoding::for_label(encoding_name.as_bytes())
            .unwrap_or(encoding_rs::UTF_8);
        let bytes = self.bytes().await?;
        let (text, _, _) = encoding.decode(&bytes);
        Ok(text.into_owned())
    }

    /// Consume the response body and deserialize it as JSON.
    #[cfg(feature = "json")]
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
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
    pub async fn upgrade(mut self) -> Result<crate::upgrade::Upgraded, Error> {
        crate::upgrade::on_upgrade(&mut self.inner).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn empty_body() -> HyperBody {
        http_body_util::Full::new(bytes::Bytes::new())
            .map_err(|never| match never {})
            .boxed()
    }

    fn make_response(status: u16) -> Response {
        let inner = http::Response::builder()
            .status(status)
            .body(empty_body())
            .unwrap();
        Response::new(inner, "http://example.com".parse().unwrap())
    }

    #[test]
    fn status_returns_correct_code() {
        let resp = make_response(200);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn url_returns_request_uri() {
        let resp = make_response(200);
        assert_eq!(resp.url().to_string(), "http://example.com/");
    }

    #[test]
    fn error_for_status_ok_on_2xx() {
        let resp = make_response(200);
        assert!(resp.error_for_status().is_ok());
    }

    #[test]
    fn error_for_status_err_on_4xx() {
        let resp = make_response(404);
        let err = resp.error_for_status().unwrap_err();
        match err {
            Error::Status(s) => assert_eq!(s, StatusCode::NOT_FOUND),
            _ => panic!("expected Error::Status"),
        }
    }

    #[test]
    fn error_for_status_err_on_5xx() {
        let resp = make_response(500);
        assert!(resp.error_for_status().is_err());
    }

    #[test]
    fn error_for_status_ref_ok_on_2xx() {
        let resp = make_response(200);
        assert!(resp.error_for_status_ref().is_ok());
    }

    #[test]
    fn error_for_status_ref_err_on_4xx() {
        let resp = make_response(403);
        assert!(resp.error_for_status_ref().is_err());
    }

    #[test]
    fn content_length_present() {
        let inner = http::Response::builder()
            .header("Content-Length", "42")
            .body(empty_body())
            .unwrap();
        let resp = Response::new(inner, "http://example.com".parse().unwrap());
        assert_eq!(resp.content_length(), Some(42));
    }

    #[test]
    fn content_length_missing() {
        let resp = make_response(200);
        assert_eq!(resp.content_length(), None);
    }

    #[test]
    fn content_length_non_numeric() {
        let inner = http::Response::builder()
            .header("Content-Length", "abc")
            .body(empty_body())
            .unwrap();
        let resp = Response::new(inner, "http://example.com".parse().unwrap());
        assert_eq!(resp.content_length(), None);
    }

    #[test]
    fn remote_addr_initially_none() {
        let resp = make_response(200);
        assert!(resp.remote_addr().is_none());
    }

    #[test]
    fn remote_addr_set_and_get() {
        let mut resp = make_response(200);
        let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        resp.set_remote_addr(Some(addr));
        assert_eq!(resp.remote_addr(), Some(addr));
    }

    #[test]
    fn version_returns_http_version() {
        let resp = make_response(200);
        assert_eq!(resp.version(), Version::HTTP_11);
    }

    #[test]
    fn headers_mut_allows_modification() {
        let mut resp = make_response(200);
        resp.headers_mut()
            .insert("x-test", "value".parse().unwrap());
        assert_eq!(resp.headers().get("x-test").unwrap(), "value");
    }

    #[test]
    fn extensions_insert_and_read() {
        let mut resp = make_response(200);
        resp.extensions_mut().insert(42u32);
        assert_eq!(resp.extensions().get::<u32>(), Some(&42));
    }

    #[test]
    fn debug_format() {
        let resp = make_response(200);
        let dbg = format!("{resp:?}");
        assert!(dbg.contains("Response"));
        assert!(dbg.contains("200"));
    }
}
