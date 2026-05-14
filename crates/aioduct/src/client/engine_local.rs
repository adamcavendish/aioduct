use http::{Method, Uri};

use super::HttpEngineLocal;
use super::builder::HttpEngineBuilder;
use crate::error::Error;
use crate::runtime::{Connector, RuntimeLocal};

impl<R: RuntimeLocal, C: Connector + Clone> HttpEngineLocal<R, C> {
    /// Create a new [`HttpEngineBuilder`] for a completion-based runtime.
    pub fn builder_local(connector: C) -> HttpEngineBuilder<R, C> {
        HttpEngineBuilder::new(connector)
    }

    /// Create a new client with default settings for a completion-based runtime.
    pub fn new_local(connector: C) -> Self {
        Self::builder_local(connector).build_local()
    }

    #[cfg(feature = "rustls")]
    /// Create a client with rustls TLS for a completion-based runtime.
    pub fn with_rustls_local(connector: C) -> Self {
        Self::builder_local(connector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build_local()
    }

    /// Start a GET request to the given URL.
    pub fn get_local(
        &self,
        uri: &str,
    ) -> Result<crate::request_local::RequestBuilderLocal<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request_local::RequestBuilderLocal::new(
            self,
            Method::GET,
            uri,
        ))
    }

    /// Start a POST request to the given URL.
    pub fn post_local(
        &self,
        uri: &str,
    ) -> Result<crate::request_local::RequestBuilderLocal<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request_local::RequestBuilderLocal::new(
            self,
            Method::POST,
            uri,
        ))
    }

    /// Start a request with the given method and URL.
    pub fn request_local(
        &self,
        method: Method,
        uri: &str,
    ) -> Result<crate::request_local::RequestBuilderLocal<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request_local::RequestBuilderLocal::new(
            self, method, uri,
        ))
    }
}
