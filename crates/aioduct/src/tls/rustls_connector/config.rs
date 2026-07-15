use std::io;
use std::pin::Pin;
use std::sync::Arc;

use rustls::pki_types::ServerName;

use super::stream::{TlsStream, poll_flush_retry, read_tls, write_tls};
use super::verifier::{NoHostnameVerifier, NoVerifier};
#[cfg(feature = "compio")]
use crate::tls::TlsConnectLocal;
use crate::tls::{Certificate, Identity, TlsConnect, crypto_provider};

/// TLS connector backed by rustls.
#[derive(Clone)]
pub struct RustlsConnector {
    pub(super) config: Arc<rustls::ClientConfig>,
}

impl RustlsConnector {
    const DEFAULT_ALPN: &[&[u8]] = &[b"h2", b"http/1.1"];

    /// Create a connector from a rustls client config.
    pub fn new(config: Arc<rustls::ClientConfig>) -> Self {
        Self { config }
    }

    pub(super) fn set_default_alpn(config: &mut rustls::ClientConfig) {
        if config.alpn_protocols.is_empty() {
            config.alpn_protocols = Self::DEFAULT_ALPN.iter().map(|p| p.to_vec()).collect();
        }
    }

    /// Get a reference to the underlying rustls config.
    pub fn config(&self) -> &Arc<rustls::ClientConfig> {
        &self.config
    }

    /// Get a mutable reference to the underlying rustls config (clones if shared).
    pub fn config_mut(&mut self) -> &mut rustls::ClientConfig {
        Arc::make_mut(&mut self.config)
    }

    /// Create a connector using WebPKI root certificates.
    pub fn with_webpki_roots() -> Self {
        Self::with_webpki_roots_versioned(&[&rustls::version::TLS12, &rustls::version::TLS13])
    }

    /// Create a connector using WebPKI root certificates with specific TLS versions.
    pub fn with_webpki_roots_versioned(
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> Self {
        let root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // SAFETY: the default crypto provider (aws-lc-rs or ring) always
        // supports TLS 1.2 and 1.3, which are the only versions callers can
        // request through the public SupportedProtocolVersion constants.
        #[allow(clippy::expect_used)]
        let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(versions)
            .expect("configured rustls provider does not support the requested TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::set_default_alpn(&mut config);
        Self::new(Arc::new(config))
    }

    /// Create a connector with WebPKI roots plus additional trusted CA certificates.
    pub fn with_extra_roots(certs: &[Certificate]) -> Self {
        Self::with_extra_roots_versioned(certs, &[&rustls::version::TLS12, &rustls::version::TLS13])
    }

    /// Create a connector with extra roots and specific TLS versions.
    pub fn with_extra_roots_versioned(
        certs: &[Certificate],
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> Self {
        let mut root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for cert in certs {
            // SAFETY: extra root certs are caller-provided; if they are
            // malformed the connector cannot be built meaningfully.
            #[allow(clippy::expect_used)]
            root_store
                .add(cert.der.clone())
                .expect("invalid extra root certificate");
        }
        // SAFETY: the default crypto provider always supports TLS 1.2 and 1.3.
        #[allow(clippy::expect_used)]
        let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(versions)
            .expect("configured rustls provider does not support the requested TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::set_default_alpn(&mut config);
        Self::new(Arc::new(config))
    }

    /// Create a connector with WebPKI roots, extra CAs, and a client identity for mutual TLS.
    pub fn with_identity(
        certs: &[Certificate],
        identity: Identity,
    ) -> std::result::Result<Self, io::Error> {
        Self::with_identity_versioned(
            certs,
            identity,
            &[&rustls::version::TLS12, &rustls::version::TLS13],
        )
    }

    /// Create a connector with identity and specific TLS versions.
    pub fn with_identity_versioned(
        certs: &[Certificate],
        identity: Identity,
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> std::result::Result<Self, io::Error> {
        let mut root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for cert in certs {
            root_store.add(cert.der.clone()).map_err(io::Error::other)?;
        }
        // SAFETY: the default crypto provider always supports TLS 1.2 and 1.3.
        #[allow(clippy::expect_used)]
        let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(versions)
            .expect("configured rustls provider does not support the requested TLS versions")
            .with_root_certificates(root_store)
            .with_client_auth_cert(identity.certs, identity.key)
            .map_err(io::Error::other)?;
        Self::set_default_alpn(&mut config);
        Ok(Self::new(Arc::new(config)))
    }

    /// Create a connector using the system's native root certificates.
    #[cfg(feature = "rustls-native-roots")]
    pub fn with_native_roots() -> Self {
        Self::with_native_roots_versioned(&[&rustls::version::TLS12, &rustls::version::TLS13])
    }

    /// Create a connector using native roots with specific TLS versions.
    #[cfg(feature = "rustls-native-roots")]
    pub fn with_native_roots_versioned(
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> Self {
        let mut root_store = rustls::RootCertStore::empty();
        let native_certs = rustls_native_certs::load_native_certs();
        // SAFETY: if the OS yields zero certs with errors, TLS cannot function
        // at all — panicking here surfaces the misconfigured system immediately.
        #[allow(clippy::panic)]
        if native_certs.certs.is_empty() && !native_certs.errors.is_empty() {
            panic!(
                "failed to load any native root certificates ({} errors)",
                native_certs.errors.len()
            );
        }
        for cert in native_certs.certs {
            let _ = root_store.add(cert);
        }
        // SAFETY: the default crypto provider always supports TLS 1.2 and 1.3.
        #[allow(clippy::expect_used)]
        let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(versions)
            .expect("configured rustls provider does not support the requested TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::set_default_alpn(&mut config);
        Self::new(Arc::new(config))
    }

    /// Create a connector that accepts any server certificate (INSECURE — testing only).
    pub fn danger_accept_invalid_certs() -> Self {
        // SAFETY: the default crypto provider always supports the safe default
        // TLS versions (1.2 and 1.3).
        #[allow(clippy::expect_used)]
        let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();
        Self::set_default_alpn(&mut config);
        Self::new(Arc::new(config))
    }

    /// Build a connector with full configuration options including CRLs and hostname override.
    pub(crate) fn build_configured(
        root_store: rustls::RootCertStore,
        versions: &[&'static rustls::SupportedProtocolVersion],
        crls: Vec<rustls::pki_types::CertificateRevocationListDer<'static>>,
        skip_hostname_verification: bool,
        identity: Option<(
            Vec<rustls::pki_types::CertificateDer<'static>>,
            rustls::pki_types::PrivateKeyDer<'static>,
        )>,
    ) -> std::result::Result<Self, io::Error> {
        if !crls.is_empty() || skip_hostname_verification {
            let mut server_verifier_builder =
                rustls::client::WebPkiServerVerifier::builder_with_provider(
                    Arc::new(root_store),
                    crypto_provider(),
                );
            if !crls.is_empty() {
                server_verifier_builder = server_verifier_builder.with_crls(crls);
            }
            let verifier = server_verifier_builder.build().map_err(io::Error::other)?;

            let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> =
                if skip_hostname_verification {
                    Arc::new(NoHostnameVerifier { inner: verifier })
                } else {
                    verifier
                };

            let config = rustls::ClientConfig::builder_with_provider(crypto_provider())
                .with_protocol_versions(versions)
                .map_err(io::Error::other)?
                .dangerous()
                .with_custom_certificate_verifier(verifier);

            let mut config = match identity {
                Some((certs, key)) => config
                    .with_client_auth_cert(certs, key)
                    .map_err(io::Error::other)?,
                None => config.with_no_client_auth(),
            };
            Self::set_default_alpn(&mut config);
            Ok(Self::new(Arc::new(config)))
        } else {
            let builder = rustls::ClientConfig::builder_with_provider(crypto_provider())
                .with_protocol_versions(versions)
                .map_err(io::Error::other)?
                .with_root_certificates(root_store);

            let mut config = match identity {
                Some((certs, key)) => builder
                    .with_client_auth_cert(certs, key)
                    .map_err(io::Error::other)?,
                None => builder.with_no_client_auth(),
            };
            Self::set_default_alpn(&mut config);
            Ok(Self::new(Arc::new(config)))
        }
    }

    pub(crate) fn negotiated_http_protocol(
        tls_conn: &rustls::ClientConnection,
    ) -> Result<Option<AlpnProtocol>, &[u8]> {
        match tls_conn.alpn_protocol() {
            Some(b"h2") => Ok(Some(AlpnProtocol::H2)),
            Some(b"http/1.1") => Ok(Some(AlpnProtocol::H1)),
            Some(protocol) => Err(protocol),
            None => Ok(None),
        }
    }

    /// Get a recognized HTTP ALPN protocol negotiated during the TLS handshake.
    ///
    /// Unknown selected protocols are returned as `None` for compatibility.
    /// Internal HTTP dispatch uses stricter validation and rejects them.
    pub fn negotiated_protocol(tls_conn: &rustls::ClientConnection) -> Option<AlpnProtocol> {
        Self::negotiated_http_protocol(tls_conn).ok().flatten()
    }
}

/// ALPN protocol negotiated during TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
pub enum AlpnProtocol {
    /// HTTP/1.1.
    H1,
    /// HTTP/2.
    H2,
}

pub(super) fn ensure_handshake_wants_read(wants_read: bool) -> io::Result<()> {
    if wants_read {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TLS handshake stalled: neither wants_read nor wants_write",
        ))
    }
}

fn new_client_connection(
    config: Arc<rustls::ClientConfig>,
    server_name: String,
    reject_ech: bool,
) -> io::Result<rustls::ClientConnection> {
    let server_name = super::super::server_name_host(&server_name).to_owned();
    let dns_name = ServerName::try_from(server_name)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let tls_conn = rustls::ClientConnection::new(config, dns_name).map_err(io::Error::other)?;
    if reject_ech && tls_conn.ech_status() != rustls::client::EchStatus::NotOffered {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HTTPS proxy TLS cannot inherit an ECH-enabled origin configuration",
        ));
    }
    Ok(tls_conn)
}

async fn connect_stream<S>(
    config: Arc<rustls::ClientConfig>,
    server_name: String,
    stream: S,
    reject_ech: bool,
) -> io::Result<TlsStream<S>>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin,
{
    let tls_conn = new_client_connection(config, server_name, reject_ech)?;
    let mut tls_stream = TlsStream::new(stream, tls_conn);

    // rustls queues the ClientHello on construction. Alternate writes and
    // reads until the handshake completes, rejecting transports that report
    // no progress while ciphertext is pending.
    while tls_stream.tls.is_handshaking() {
        while tls_stream.tls.wants_write() {
            std::future::poll_fn(|cx| write_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx))
                .await?;
        }
        std::future::poll_fn(|cx| poll_flush_retry(&mut tls_stream.inner, cx)).await?;
        let wants_read = tls_stream.tls.wants_read();
        ensure_handshake_wants_read(wants_read)?;
        let n = std::future::poll_fn(|cx| read_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx))
            .await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "TLS handshake: peer closed connection",
            ));
        }
        tls_stream
            .tls
            .process_new_packets()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    }

    // Flush TLS 1.3 client Finished and any other trailing handshake data.
    while tls_stream.tls.wants_write() {
        std::future::poll_fn(|cx| write_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx))
            .await?;
    }
    std::future::poll_fn(|cx| poll_flush_retry(&mut tls_stream.inner, cx)).await?;

    Ok(tls_stream)
}

impl<S> TlsConnect<S> for RustlsConnector
where
    S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
{
    type Stream = TlsStream<S>;

    fn connect(
        &self,
        server_name: &str,
        stream: S,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<Self::Stream>> + Send + '_>> {
        let server_name = server_name.to_owned();
        let config = Arc::clone(&self.config);
        Box::pin(connect_stream(config, server_name, stream, false))
    }
}

impl RustlsConnector {
    pub(crate) fn preflight_https_proxy(&self, server_name: &str) -> io::Result<()> {
        let mut config = self.config.as_ref().clone();
        // A validation-only connection must not consume a cached origin session ticket.
        config.resumption = rustls::client::Resumption::disabled();
        new_client_connection(Arc::new(config), server_name.to_owned(), true).map(drop)
    }

    pub(crate) fn connect_https_proxy_send<S>(
        &self,
        server_name: &str,
        stream: S,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<TlsStream<S>>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let server_name = server_name.to_owned();
        let config = Arc::clone(&self.config);
        Box::pin(connect_stream(config, server_name, stream, true))
    }

    #[cfg(feature = "compio")]
    pub(crate) fn connect_https_proxy_local<S>(
        &self,
        server_name: &str,
        stream: S,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<TlsStream<S>>> + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let server_name = server_name.to_owned();
        let config = Arc::clone(&self.config);
        Box::pin(connect_stream(config, server_name, stream, true))
    }
}

#[cfg(feature = "compio")]
impl<S> TlsConnectLocal<S> for RustlsConnector
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
{
    type Stream = TlsStream<S>;

    fn connect_local(
        &self,
        server_name: &str,
        stream: S,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<Self::Stream>> + '_>> {
        let server_name = server_name.to_owned();
        let config = Arc::clone(&self.config);
        Box::pin(connect_stream(config, server_name, stream, false))
    }
}
