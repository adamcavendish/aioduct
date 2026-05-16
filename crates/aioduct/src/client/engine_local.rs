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
    ) -> Result<crate::request::RequestBuilderLocal<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request::RequestBuilderLocal::new(
            self,
            Method::GET,
            uri,
        ))
    }

    /// Start a POST request to the given URL.
    pub fn post_local(
        &self,
        uri: &str,
    ) -> Result<crate::request::RequestBuilderLocal<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request::RequestBuilderLocal::new(
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
    ) -> Result<crate::request::RequestBuilderLocal<'_, R, C>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(crate::request::RequestBuilderLocal::new(self, method, uri))
    }

    /// Start a parallel chunk download for the given URL.
    pub fn chunk_download_local(
        &self,
        url: &str,
    ) -> crate::chunk_download::ChunkDownloadLocal<R, C> {
        crate::chunk_download::ChunkDownloadLocal::new(self.clone(), url.to_owned())
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

#[cfg(all(test, feature = "compio"))]
mod tests {
    use crate::client::HttpEngineLocal;
    use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};

    fn test_client() -> HttpEngineLocal<CompioRuntime, TcpConnector> {
        HttpEngineLocal::new_local(TcpConnector)
    }

    #[cfg(feature = "rustls")]
    fn install_crypto() {
        crate::tls::install_default_crypto_provider();
    }

    #[test]
    fn get_local_valid_url() {
        let client = test_client();
        assert!(client.get_local("http://example.com").is_ok());
    }

    #[test]
    fn get_local_invalid_url() {
        let client = test_client();
        assert!(client.get_local("not a url\n").is_err());
    }

    #[test]
    fn post_local_valid_url() {
        let client = test_client();
        assert!(client.post_local("http://example.com").is_ok());
    }

    #[test]
    fn request_local_valid_url() {
        let client = test_client();
        assert!(
            client
                .request_local(http::Method::PUT, "http://example.com")
                .is_ok()
        );
    }

    #[test]
    fn request_local_invalid_url() {
        let client = test_client();
        assert!(
            client
                .request_local(http::Method::PUT, "not valid\n")
                .is_err()
        );
    }

    // ── build_local() TLS path coverage ──────────────────────────────────

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_explicit_passthrough() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_version_constraints_only() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_max_version_only() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_min_and_max() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_extra_root_certs() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .add_root_certificates(&[cert])
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_extra_root_certs_with_version() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .add_root_certificates(&[cert])
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_identity() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .identity(id)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_identity_with_version_constraints() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .identity(id)
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_danger_accept_invalid_hostnames() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .danger_accept_invalid_hostnames(true)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_danger_invalid_hostnames_with_identity() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .identity(id)
            .danger_accept_invalid_hostnames(true)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_danger_invalid_hostnames_with_extra_roots() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .add_root_certificates(&[cert])
            .danger_accept_invalid_hostnames(true)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_sni_disabled() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .tls_sni(false)
            .build_local();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_danger_accept_invalid_certs() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .danger_accept_invalid_certs()
            .build_local();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_no_config_uses_default_webpki() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .build_local();
        assert!(client.core.tls.is_some());
    }

    // ── build_local() resolver path coverage ─────────────────────────────

    #[test]
    fn build_local_static_resolver_setup() {
        let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .resolve("example.com", addr)
            .build_local();
        assert!(client.core.resolver.is_some());
    }

    #[test]
    fn build_local_static_resolver_multiple_hosts() {
        let addr1: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let addr2: std::net::SocketAddr = "127.0.0.1:9090".parse().unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .resolve("example.com", addr1)
            .resolve("other.com", addr2)
            .build_local();
        assert!(client.core.resolver.is_some());
    }

    #[test]
    fn build_local_no_connection_reuse() {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder_local(TcpConnector)
            .no_connection_reuse()
            .build_local();
        assert!(client.core.no_connection_reuse);
    }
}
