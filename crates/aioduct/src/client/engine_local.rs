use http::{Method, Uri};

use super::HttpEngineLocal;
use super::builder::HttpEngineBuilder;
use super::extract_fragment;
use crate::error::Error;
use crate::runtime::{ConnectorLocal, RuntimeLocal};

impl<R: RuntimeLocal, C: ConnectorLocal + Clone + Default> HttpEngineLocal<R, C> {
    /// Create a new client with default settings for a completion-based runtime.
    pub fn new() -> Self {
        Self::with_connector(C::default())
    }

    /// Create a new [`HttpEngineBuilder`] for a completion-based runtime.
    pub fn builder() -> HttpEngineBuilder<R, C> {
        Self::builder_with_connector(C::default())
    }

    #[cfg(feature = "rustls")]
    /// Create a client with rustls TLS for a completion-based runtime.
    pub fn with_rustls() -> Self {
        Self::with_rustls_connector(C::default())
    }
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone + Default> Default for HttpEngineLocal<R, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> HttpEngineLocal<R, C> {
    /// Create a new [`HttpEngineBuilder`] with a specific connector for a completion-based runtime.
    pub fn builder_with_connector(connector: C) -> HttpEngineBuilder<R, C> {
        HttpEngineBuilder::new(connector)
    }

    /// Create a new client with default settings and a specific connector.
    #[allow(clippy::expect_used)]
    pub fn with_connector(connector: C) -> Self {
        Self::builder_with_connector(connector)
            .build_local()
            .expect("default build_local")
    }

    #[cfg(feature = "rustls")]
    #[allow(clippy::expect_used)]
    /// Create a client with rustls TLS and a specific connector for a completion-based runtime.
    pub fn with_rustls_connector(connector: C) -> Self {
        Self::builder_with_connector(connector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build_local()
            .expect("rustls build_local")
    }

    /// Start a GET request to the given URL.
    pub fn get_local(
        &self,
        uri: &str,
    ) -> Result<crate::request::RequestBuilderLocal<'_, R, C>, Error> {
        let fragment = extract_fragment(uri);
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request::RequestBuilderLocal::new(
            self,
            Method::GET,
            uri,
            fragment,
        ))
    }

    /// Start a POST request to the given URL.
    pub fn post_local(
        &self,
        uri: &str,
    ) -> Result<crate::request::RequestBuilderLocal<'_, R, C>, Error> {
        let fragment = extract_fragment(uri);
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request::RequestBuilderLocal::new(
            self,
            Method::POST,
            uri,
            fragment,
        ))
    }

    /// Start a request with the given method and URL.
    pub fn request_local(
        &self,
        method: Method,
        uri: &str,
    ) -> Result<crate::request::RequestBuilderLocal<'_, R, C>, Error> {
        let fragment = extract_fragment(uri);
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request::RequestBuilderLocal::new(
            self, method, uri, fragment,
        ))
    }

    /// Start a parallel chunk download for the given URL.
    pub fn chunk_download_local(
        &self,
        url: &str,
    ) -> crate::chunk_download::ChunkDownloadLocal<R, C> {
        crate::chunk_download::ChunkDownloadLocal::new(self.clone(), url.to_owned())
    }

    /// Resolve a hostname to all socket addresses using the configured DNS resolver.
    ///
    /// Returns every address the resolver provides. This enables service discovery
    /// and custom load-balancing: resolve once, select an address with your
    /// strategy (round-robin, least-connections, consistent hashing), then send the
    /// request to the chosen address via
    /// [`crate::RequestBuilderLocal::force_addr`].
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
    pub fn forward_local<B>(
        &self,
        request: http::Request<B>,
    ) -> crate::forward::forward_local::ForwardBuilderLocal<'_, R, C, B>
    where
        B: http_body::Body<Data = bytes::Bytes> + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        crate::forward::forward_local::ForwardBuilderLocal::new(self, request)
    }
}
