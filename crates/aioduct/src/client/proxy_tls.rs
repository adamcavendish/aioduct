use crate::error::Error;
use crate::proxy::{ProxyEstablishmentPlan, ProxyScheme};
use crate::tls::{RustlsConnector, TlsStream};

fn http1_connector(connector: &RustlsConnector) -> RustlsConnector {
    let mut connector = connector.clone();
    let config = connector.config_mut();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    connector
}

fn validate_http1_alpn<S>(stream: &TlsStream<S>) -> Result<(), Error> {
    match stream.tls_connection().alpn_protocol() {
        Some(b"http/1.1") => Ok(()),
        Some(protocol) => Err(Error::Tls(
            format!(
                "HTTPS proxy negotiated unsupported ALPN protocol `{}`; textual CONNECT requires `http/1.1`",
                String::from_utf8_lossy(protocol)
            )
            .into(),
        )),
        // RFC 7301 leaves ALPN optional. Because this connector advertises only
        // HTTP/1.1, no negotiated protocol falls back to HTTP/1.1 semantics.
        None => Ok(()),
    }
}

pub(super) fn preflight_https_proxy_hops(
    connector: &RustlsConnector,
    plan: &ProxyEstablishmentPlan,
) -> Result<(), Error> {
    for hop in [Some(plan.first()), plan.second()].into_iter().flatten() {
        if hop.scheme() == &ProxyScheme::Https {
            connector
                .preflight_https_proxy(hop.endpoint().host())
                .map_err(|error| Error::Tls(Box::new(error)))?;
        }
    }
    Ok(())
}

pub(super) async fn connect_send<S>(
    connector: &RustlsConnector,
    host: &str,
    stream: S,
) -> Result<TlsStream<S>, Error>
where
    S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
{
    let connector = http1_connector(connector);
    let stream = connector
        .connect_https_proxy_send(host, stream)
        .await
        .map_err(|error| Error::Tls(Box::new(error)))?;
    validate_http1_alpn(&stream)?;
    Ok(stream)
}

#[cfg(feature = "compio")]
pub(super) async fn connect_local<S>(
    connector: &RustlsConnector,
    host: &str,
    stream: S,
) -> Result<TlsStream<S>, Error>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    let connector = http1_connector(connector);
    let stream = connector
        .connect_https_proxy_local(host, stream)
        .await
        .map_err(|error| Error::Tls(Box::new(error)))?;
    validate_http1_alpn(&stream)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[cfg(feature = "rustls-aws-lc-rs")]
    #[derive(Debug)]
    struct PanicOnProxyTlsIo;

    #[cfg(feature = "rustls-aws-lc-rs")]
    impl hyper::rt::Read for PanicOnProxyTlsIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            panic!("ECH proxy rejection must happen before reading from the transport")
        }
    }

    #[cfg(feature = "rustls-aws-lc-rs")]
    impl hyper::rt::Write for PanicOnProxyTlsIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            panic!("ECH proxy rejection must happen before writing to the transport")
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            panic!("ECH proxy rejection must happen before flushing the transport")
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            panic!("ECH proxy rejection must happen before shutting down the transport")
        }
    }

    #[cfg(feature = "rustls-aws-lc-rs")]
    fn ech_grease_connector() -> RustlsConnector {
        use rustls::crypto::hpke::Hpke as _;

        let hpke = rustls::crypto::aws_lc_rs::hpke::DH_KEM_P256_HKDF_SHA256_AES_128;
        let (placeholder_key, _) = hpke.generate_key_pair().expect("HPKE key pair");
        let ech_mode = rustls::client::EchMode::Grease(rustls::client::EchGreaseConfig::new(
            hpke,
            placeholder_key,
        ));
        let root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(crate::tls::crypto_provider())
            .with_ech(ech_mode)
            .expect("ECH config")
            .with_root_certificates(root_store)
            .with_no_client_auth();
        RustlsConnector::new(Arc::new(config))
    }

    #[cfg(feature = "rustls-aws-lc-rs")]
    fn assert_ech_proxy_rejection(error: Error) {
        assert!(
            matches!(error, Error::Tls(ref source) if source.to_string().contains("cannot inherit an ECH-enabled origin configuration")),
            "{error}"
        );
    }

    #[test]
    fn proxy_connector_advertises_only_http1() {
        crate::tls::install_default_crypto_provider();
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let connector = http1_connector(&connector);

        assert_eq!(
            connector.config().alpn_protocols,
            vec![b"http/1.1".to_vec()]
        );
    }

    #[derive(Debug)]
    struct ConfiguredOriginIdentity;

    impl rustls::client::ResolvesClientCert for ConfiguredOriginIdentity {
        fn resolve(
            &self,
            _root_hint_subjects: &[&[u8]],
            _sigschemes: &[rustls::SignatureScheme],
        ) -> Option<Arc<rustls::sign::CertifiedKey>> {
            None
        }

        fn has_certs(&self) -> bool {
            true
        }
    }

    #[test]
    fn proxy_connector_preserves_configured_client_identity() {
        crate::tls::install_default_crypto_provider();
        let mut connector = RustlsConnector::danger_accept_invalid_certs();
        connector.config_mut().enable_sni = false;
        connector.config_mut().client_auth_cert_resolver = Arc::new(ConfiguredOriginIdentity);

        let proxy_connector = http1_connector(&connector);

        assert!(connector.config().client_auth_cert_resolver.has_certs());
        assert!(
            proxy_connector
                .config()
                .client_auth_cert_resolver
                .has_certs()
        );
        assert!(!proxy_connector.config().enable_sni);
    }

    #[cfg(feature = "rustls-aws-lc-rs")]
    #[test]
    fn send_proxy_tls_rejects_origin_ech_before_transport_io() {
        use futures_util::FutureExt as _;

        let connector = ech_grease_connector();
        let result = connect_send(&connector, "proxy.example", PanicOnProxyTlsIo)
            .now_or_never()
            .expect("ECH rejection must not yield");
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("origin ECH must not be reused for an HTTPS proxy"),
        };
        assert_ech_proxy_rejection(error);
    }

    #[cfg(all(feature = "rustls-aws-lc-rs", feature = "compio"))]
    #[test]
    fn local_proxy_tls_rejects_origin_ech_before_transport_io() {
        use futures_util::FutureExt as _;

        let connector = ech_grease_connector();
        let result = connect_local(&connector, "proxy.example", PanicOnProxyTlsIo)
            .now_or_never()
            .expect("ECH rejection must not yield");
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("origin ECH must not be reused for an HTTPS proxy"),
        };
        assert_ech_proxy_rejection(error);
    }
}
