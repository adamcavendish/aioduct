use http::{Method, Uri};

use super::HttpEngineSend;
use super::builder::HttpEngineBuilder;
use super::resolve_request_url;
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

    /// Resolve a request input URL against the configured base URL, if any.
    fn resolve_url(&self, uri: &str) -> Result<(Uri, Option<String>), Error> {
        resolve_request_url(self.core.base_url.as_deref(), uri)
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
        let (uri, fragment) = self.resolve_url(uri)?;
        Ok(RequestBuilderSend::new(self, Method::GET, uri, fragment))
    }

    /// Start a HEAD request to the given URL.
    pub fn head(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let (uri, fragment) = self.resolve_url(uri)?;
        Ok(RequestBuilderSend::new(self, Method::HEAD, uri, fragment))
    }

    /// Start a POST request to the given URL.
    pub fn post(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let (uri, fragment) = self.resolve_url(uri)?;
        Ok(RequestBuilderSend::new(self, Method::POST, uri, fragment))
    }

    /// Start a PUT request to the given URL.
    pub fn put(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let (uri, fragment) = self.resolve_url(uri)?;
        Ok(RequestBuilderSend::new(self, Method::PUT, uri, fragment))
    }

    /// Start a PATCH request to the given URL.
    pub fn patch(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let (uri, fragment) = self.resolve_url(uri)?;
        Ok(RequestBuilderSend::new(self, Method::PATCH, uri, fragment))
    }

    /// Start a DELETE request to the given URL.
    pub fn delete(&self, uri: &str) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let (uri, fragment) = self.resolve_url(uri)?;
        Ok(RequestBuilderSend::new(self, Method::DELETE, uri, fragment))
    }

    /// Start a request with the given method and URL.
    pub fn request(
        &self,
        method: Method,
        uri: &str,
    ) -> Result<RequestBuilderSend<'_, R, C>, Error> {
        let (uri, fragment) = self.resolve_url(uri)?;
        Ok(RequestBuilderSend::new(self, method, uri, fragment))
    }

    /// Start a parallel chunk download for the given URL.
    pub fn chunk_download(&self, url: &str) -> crate::chunk_download::ChunkDownload<R, C> {
        crate::chunk_download::ChunkDownload::new(self.clone(), url.to_owned())
    }

    /// Resolve a hostname to all socket addresses using the configured DNS resolver.
    ///
    /// Returns every address the resolver provides. This enables service discovery
    /// and custom load-balancing: resolve once, select an address with your
    /// strategy (round-robin, least-connections, consistent hashing), then send the
    /// request to the chosen address via
    /// [`crate::RequestBuilderSend::force_addr`].
    ///
    /// For a host that is already an IP literal, this returns a single-element vec
    /// without consulting any resolver.
    pub async fn resolve_all(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, Error> {
        self.core.resolve_all_authority_raw(host, port).await
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

    pub(crate) fn default_connect_timeout(&self) -> Option<std::time::Duration> {
        self.core.connect_timeout
    }

    pub(crate) fn default_write_timeout(&self) -> Option<std::time::Duration> {
        self.core.write_timeout
    }

    pub(crate) fn default_read_timeout(&self) -> Option<std::time::Duration> {
        self.core.read_timeout
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
