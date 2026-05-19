use http::{Method, Uri};

use super::HttpEngineSend;
use super::builder::HttpEngineBuilder;
use crate::error::Error;
use crate::request::RequestBuilderSend;
use crate::runtime::{ConnectorSend, RuntimePoll};

impl<R: RuntimePoll, C: ConnectorSend + Default> Default for HttpEngineSend<R, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: RuntimePoll, C: ConnectorSend + Default> HttpEngineSend<R, C> {
    /// Create a new client with default settings and the default connector.
    pub fn new() -> Self {
        Self::with_connector(C::default())
    }

    /// Create a new [`HttpEngineBuilder`] with default settings and the default connector.
    pub fn builder() -> HttpEngineBuilder<R, C> {
        Self::builder_with_connector(C::default())
    }

    #[cfg(feature = "rustls")]
    /// Create a client with rustls TLS using WebPKI root certificates.
    pub fn with_rustls() -> Self {
        Self::with_rustls_connector(C::default())
    }

    #[cfg(feature = "rustls-native-roots")]
    /// Create a client with rustls TLS using the system's native root certificates.
    pub fn with_native_roots() -> Self {
        Self::with_native_roots_connector(C::default())
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Create a client configured for HTTP/3 with rustls.
    pub fn with_http3() -> Result<Self, Error> {
        Self::with_http3_connector(C::default())
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Create a client that upgrades to HTTP/3 via Alt-Svc discovery.
    pub fn with_alt_svc_h3() -> Result<Self, Error> {
        Self::with_alt_svc_h3_connector(C::default())
    }
}

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    /// Create a new [`HttpEngineBuilder`] with a specific connector.
    pub fn builder_with_connector(connector: C) -> HttpEngineBuilder<R, C> {
        HttpEngineBuilder::new(connector)
    }

    /// Create a new client with default settings and a specific connector.
    #[allow(clippy::expect_used)]
    pub fn with_connector(connector: C) -> Self {
        Self::builder_with_connector(connector)
            .build()
            .expect("default build")
    }

    #[cfg(feature = "rustls")]
    #[allow(clippy::expect_used)]
    /// Create a client with rustls TLS using WebPKI root certificates and a specific connector.
    pub fn with_rustls_connector(connector: C) -> Self {
        Self::builder_with_connector(connector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build()
            .expect("rustls build")
    }

    #[cfg(feature = "rustls-native-roots")]
    #[allow(clippy::expect_used)]
    /// Create a client with rustls TLS using native root certificates and a specific connector.
    pub fn with_native_roots_connector(connector: C) -> Self {
        Self::builder_with_connector(connector)
            .tls(crate::tls::RustlsConnector::with_native_roots())
            .build()
            .expect("native-roots build")
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Create a client configured for HTTP/3 with rustls and a specific connector.
    pub fn with_http3_connector(connector: C) -> Result<Self, Error> {
        Self::builder_with_connector(connector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)?
            .build()
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Create a client that upgrades to HTTP/3 via Alt-Svc discovery with a specific connector.
    pub fn with_alt_svc_h3_connector(connector: C) -> Result<Self, Error> {
        Self::builder_with_connector(connector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .alt_svc_h3(true)?
            .build()
    }

    /// Start a GET request to the given URL.
    pub fn get(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilderSend::new(self, Method::GET, uri))
    }

    /// Start a HEAD request to the given URL.
    pub fn head(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilderSend::new(self, Method::HEAD, uri))
    }

    /// Start a POST request to the given URL.
    pub fn post(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilderSend::new(self, Method::POST, uri))
    }

    /// Start a PUT request to the given URL.
    pub fn put(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilderSend::new(self, Method::PUT, uri))
    }

    /// Start a PATCH request to the given URL.
    pub fn patch(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilderSend::new(self, Method::PATCH, uri))
    }

    /// Start a DELETE request to the given URL.
    pub fn delete(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilderSend::new(self, Method::DELETE, uri))
    }

    /// Start a request with the given method and URL.
    pub fn request(
        &self,
        method: Method,
        uri: &str,
    ) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilderSend::new(self, method, uri))
    }

    /// Start a parallel chunk download for the given URL.
    pub fn chunk_download(&self, url: &str) -> crate::chunk_download::ChunkDownload<R, C> {
        crate::chunk_download::ChunkDownload::new(self.clone(), url.to_owned())
    }

    /// Forward an incoming HTTP request to an upstream server.
    pub fn forward<B>(
        &self,
        request: http::Request<B>,
    ) -> crate::forward::ForwardBuilder<'_, R, C, B>
    where
        B: http_body::Body<Data = bytes::Bytes> + Send + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        crate::forward::ForwardBuilder::new(self, request)
    }

    pub(crate) fn default_timeout(&self) -> Option<std::time::Duration> {
        self.core.timeout
    }

    pub(crate) fn default_retry(&self) -> Option<&crate::retry::RetryConfig> {
        self.core.retry.as_ref()
    }

    pub(crate) fn middleware(&self) -> &crate::middleware::MiddlewareStack {
        &self.core.middleware
    }

    /// Returns the bandwidth limiter if one was configured.
    pub fn bandwidth_limiter(&self) -> Option<&crate::bandwidth::BandwidthLimiter> {
        self.core.bandwidth_limiter.as_ref()
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use crate::client::HttpEngineSend;
    use crate::runtime::tokio_rt::TokioRuntime;

    fn test_client() -> HttpEngineSend<TokioRuntime, crate::runtime::tokio_rt::TcpConnector> {
        HttpEngineSend::new()
    }

    #[test]
    fn get_valid_url() {
        let client = test_client();
        assert!(client.get("http://example.com").is_ok());
    }

    #[test]
    fn head_valid_url() {
        let client = test_client();
        assert!(client.head("http://example.com").is_ok());
    }

    #[test]
    fn post_valid_url() {
        let client = test_client();
        assert!(client.post("http://example.com").is_ok());
    }

    #[test]
    fn put_valid_url() {
        let client = test_client();
        assert!(client.put("http://example.com").is_ok());
    }

    #[test]
    fn patch_valid_url() {
        let client = test_client();
        assert!(client.patch("http://example.com").is_ok());
    }

    #[test]
    fn delete_valid_url() {
        let client = test_client();
        assert!(client.delete("http://example.com").is_ok());
    }

    #[test]
    fn request_valid_url() {
        let client = test_client();
        assert!(
            client
                .request(http::Method::OPTIONS, "http://example.com")
                .is_ok()
        );
    }

    #[test]
    fn get_invalid_url() {
        let client = test_client();
        assert!(client.get("not a url\n").is_err());
    }

    #[test]
    fn default_timeout_is_none() {
        let client = test_client();
        assert!(client.default_timeout().is_none());
    }

    #[test]
    fn default_retry_is_none() {
        let client = test_client();
        assert!(client.default_retry().is_none());
    }

    #[test]
    fn bandwidth_limiter_is_none() {
        let client = test_client();
        assert!(client.bandwidth_limiter().is_none());
    }
}
