#![cfg(all(test, feature = "compio"))]
#[cfg(all(test, feature = "compio"))]
mod tests {
    use crate::client::HttpEngineLocal;
    use crate::runtime::compio_rt::{CompioRuntime, TcpConnector};

    fn test_client() -> HttpEngineLocal<CompioRuntime, crate::runtime::compio_rt::TcpConnector> {
        HttpEngineLocal::new()
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
        assert!(
            client
                .get_local(
                    "not a url
"
                )
                .is_err()
        );
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
                .request_local(
                    http::Method::PUT,
                    "not valid
"
                )
                .is_err()
        );
    }

    // ── build_local() TLS path coverage ──────────────────────────────────

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_explicit_passthrough() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_version_constraints_only() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_max_version_only() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_min_and_max() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_extra_root_certs() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[cert])
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_extra_root_certs_with_version() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[cert])
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local()
            .unwrap();
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .identity(id)
            .build_local()
            .unwrap();
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .identity(id)
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_danger_accept_invalid_hostnames() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .danger_accept_invalid_hostnames(true)
            .build_local()
            .unwrap();
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
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .identity(id)
            .danger_accept_invalid_hostnames(true)
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_danger_invalid_hostnames_with_extra_roots() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .add_root_certificates(&[cert])
            .danger_accept_invalid_hostnames(true)
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_sni_disabled() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls_sni(false)
            .build_local()
            .unwrap();
        let tls = client.core.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_danger_accept_invalid_certs() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .danger_accept_invalid_certs()
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[test]
    fn build_local_tls_no_config_uses_default_webpki() {
        install_crypto();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .build_local()
            .unwrap();
        assert!(client.core.tls.is_some());
    }

    // ── build_local() resolver path coverage ─────────────────────────────

    #[test]
    fn build_local_static_resolver_setup() {
        let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolve("example.com", addr)
            .build_local()
            .unwrap();
        assert!(client.core.resolver.is_some());
    }

    #[test]
    fn build_local_static_resolver_multiple_hosts() {
        let addr1: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let addr2: std::net::SocketAddr = "127.0.0.1:9090".parse().unwrap();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolve("example.com", addr1)
            .resolve("other.com", addr2)
            .build_local()
            .unwrap();
        assert!(client.core.resolver.is_some());
    }

    #[test]
    fn build_local_no_connection_reuse() {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .no_connection_reuse()
            .build_local()
            .unwrap();
        assert!(client.core.no_connection_reuse);
    }
}
