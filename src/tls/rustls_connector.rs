use std::io;
use std::io::{Read as StdRead, Write as StdWrite};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper::rt::{self, Read, Write};
use rustls::pki_types::ServerName;

use super::TlsConnect;
use crate::runtime::Runtime;

/// TLS connector backed by rustls.
#[derive(Clone)]
pub struct RustlsConnector {
    config: Arc<rustls::ClientConfig>,
}

impl RustlsConnector {
    /// Create a connector from a rustls client config.
    pub fn new(config: Arc<rustls::ClientConfig>) -> Self {
        Self { config }
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
        let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::new(Arc::new(config))
    }

    /// Create a connector with WebPKI roots plus additional trusted CA certificates.
    pub fn with_extra_roots(certs: &[super::Certificate]) -> Self {
        Self::with_extra_roots_versioned(certs, &[&rustls::version::TLS12, &rustls::version::TLS13])
    }

    /// Create a connector with extra roots and specific TLS versions.
    pub fn with_extra_roots_versioned(
        certs: &[super::Certificate],
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> Self {
        let mut root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for cert in certs {
            let _ = root_store.add(cert.der.clone());
        }
        let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::new(Arc::new(config))
    }

    /// Create a connector with WebPKI roots, extra CAs, and a client identity for mutual TLS.
    pub fn with_identity(
        certs: &[super::Certificate],
        identity: super::Identity,
    ) -> std::result::Result<Self, io::Error> {
        Self::with_identity_versioned(
            certs,
            identity,
            &[&rustls::version::TLS12, &rustls::version::TLS13],
        )
    }

    /// Create a connector with identity and specific TLS versions.
    pub fn with_identity_versioned(
        certs: &[super::Certificate],
        identity: super::Identity,
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> std::result::Result<Self, io::Error> {
        let mut root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for cert in certs {
            let _ = root_store.add(cert.der.clone());
        }
        let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(root_store)
            .with_client_auth_cert(identity.certs, identity.key)
            .map_err(io::Error::other)?;
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
        for cert in native_certs {
            let _ = root_store.add(cert);
        }
        let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::new(Arc::new(config))
    }

    /// Create a connector that accepts any server certificate (INSECURE — testing only).
    pub fn danger_accept_invalid_certs() -> Self {
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();
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
                rustls::client::WebPkiServerVerifier::builder(Arc::new(root_store));
            if !crls.is_empty() {
                server_verifier_builder = server_verifier_builder.with_crls(crls);
            }
            let verifier = server_verifier_builder
                .build()
                .map_err(io::Error::other)?;

            let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> =
                if skip_hostname_verification {
                    Arc::new(NoHostnameVerifier { inner: verifier })
                } else {
                    verifier
                };

            let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
                .dangerous()
                .with_custom_certificate_verifier(verifier);

            let config = match identity {
                Some((certs, key)) => config
                    .with_client_auth_cert(certs, key)
                    .map_err(io::Error::other)?,
                None => config.with_no_client_auth(),
            };
            Ok(Self::new(Arc::new(config)))
        } else {
            let builder = rustls::ClientConfig::builder_with_protocol_versions(versions)
                .with_root_certificates(root_store);

            let config = match identity {
                Some((certs, key)) => builder
                    .with_client_auth_cert(certs, key)
                    .map_err(io::Error::other)?,
                None => builder.with_no_client_auth(),
            };
            Ok(Self::new(Arc::new(config)))
        }
    }

    /// Get the ALPN protocol negotiated during the TLS handshake.
    pub fn negotiated_protocol(tls_conn: &rustls::ClientConnection) -> Option<AlpnProtocol> {
        tls_conn.alpn_protocol().and_then(|proto| {
            if proto == b"h2" {
                Some(AlpnProtocol::H2)
            } else if proto == b"http/1.1" {
                Some(AlpnProtocol::H1)
            } else {
                None
            }
        })
    }
}

/// ALPN protocol negotiated during TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlpnProtocol {
    /// HTTP/1.1.
    H1,
    /// HTTP/2.
    H2,
}

impl<R: Runtime> TlsConnect<R> for RustlsConnector {
    type Stream = TlsStream<R::TcpStream>;

    fn connect(
        &self,
        server_name: &str,
        stream: R::TcpStream,
    ) -> Pin<Box<dyn std::future::Future<Output = io::Result<Self::Stream>> + Send + '_>> {
        let server_name = server_name.to_owned();
        let config = Arc::clone(&self.config);
        Box::pin(async move {
            let dns_name = ServerName::try_from(server_name)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let tls_conn =
                rustls::ClientConnection::new(config, dns_name).map_err(io::Error::other)?;
            Ok(TlsStream::new(stream, tls_conn))
        })
    }
}

/// A TLS-wrapped stream implementing hyper's `Read` and `Write`.
pub struct TlsStream<S> {
    inner: S,
    tls: rustls::ClientConnection,
}

impl<S> TlsStream<S> {
    /// Create a TLS stream wrapping the given transport and connection.
    pub fn new(inner: S, tls: rustls::ClientConnection) -> Self {
        Self { inner, tls }
    }

    /// Get a reference to the underlying rustls connection.
    pub fn tls_connection(&self) -> &rustls::ClientConnection {
        &self.tls
    }

    /// Extract TLS handshake info (peer certificate, etc.).
    pub fn tls_info(&self) -> super::TlsInfo {
        super::TlsInfo::from_rustls(&self.tls)
    }
}

impl<S: Unpin> Unpin for TlsStream<S> {}

impl<S> Read for TlsStream<S>
where
    S: Read + Write + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // First, try to read any buffered plaintext from rustls
        let plaintext_slice = unsafe {
            let uninit = buf.as_mut();
            std::slice::from_raw_parts_mut(uninit.as_mut_ptr() as *mut u8, uninit.len())
        };

        match this.tls.reader().read(plaintext_slice) {
            Ok(n) => {
                unsafe { buf.advance(n) };
                return Poll::Ready(Ok(()));
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Poll::Ready(Err(e)),
        }

        // Feed ciphertext from the underlying stream into rustls
        match read_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(0)) => return Poll::Ready(Ok(())),
            Poll::Ready(Ok(_n)) => {
                this.tls
                    .process_new_packets()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            }
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }

        // Try reading plaintext again after processing
        match this.tls.reader().read(plaintext_slice) {
            Ok(n) => {
                unsafe { buf.advance(n) };
                Poll::Ready(Ok(()))
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl<S> Write for TlsStream<S>
where
    S: Read + Write + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();

        // Write plaintext into rustls
        let n = match this.tls.writer().write(buf) {
            Ok(n) => n,
            Err(e) => return Poll::Ready(Err(e)),
        };

        // Flush ciphertext to the underlying stream
        match write_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(n)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Ready(Ok(n)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // Flush any remaining ciphertext
        match write_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(_)) => {
                // Also flush the underlying stream
                Pin::new(&mut this.inner).poll_flush(cx)
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.tls.send_close_notify();

        // Flush the close_notify
        match write_tls(&mut this.tls, &mut this.inner, cx) {
            Poll::Ready(Ok(_)) => Pin::new(&mut this.inner).poll_shutdown(cx),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Read ciphertext from the async stream into rustls.
fn read_tls<S: Read + Unpin>(
    tls: &mut rustls::ClientConnection,
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<io::Result<usize>> {
    struct AsyncReader<'a, 'b, S> {
        stream: &'a mut S,
        cx: &'a mut Context<'b>,
    }

    impl<S: Read + Unpin> StdRead for AsyncReader<'_, '_, S> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut read_buf = rt::ReadBuf::new(buf);
            match Pin::new(&mut *self.stream).poll_read(self.cx, read_buf.unfilled()) {
                Poll::Ready(Ok(())) => Ok(read_buf.filled().len()),
                Poll::Ready(Err(e)) => Err(e),
                Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }
    }

    let mut reader = AsyncReader { stream, cx };
    match tls.read_tls(&mut reader) {
        Ok(n) => Poll::Ready(Ok(n)),
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
        Err(e) => Poll::Ready(Err(e)),
    }
}

/// Write ciphertext from rustls to the async stream.
fn write_tls<S: Write + Unpin>(
    tls: &mut rustls::ClientConnection,
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<io::Result<usize>> {
    struct AsyncWriter<'a, 'b, S> {
        stream: &'a mut S,
        cx: &'a mut Context<'b>,
    }

    impl<S: Write + Unpin> StdWrite for AsyncWriter<'_, '_, S> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match Pin::new(&mut *self.stream).poll_write(self.cx, buf) {
                Poll::Ready(r) => r,
                Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            match Pin::new(&mut *self.stream).poll_flush(self.cx) {
                Poll::Ready(r) => r,
                Poll::Pending => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }
    }

    let mut writer = AsyncWriter { stream, cx };
    match tls.write_tls(&mut writer) {
        Ok(n) => Poll::Ready(Ok(n)),
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
        Err(e) => Poll::Ready(Err(e)),
    }
}

#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::CryptoProvider::get_default()
            .map(|p| p.signature_verification_algorithms.supported_schemes())
            .unwrap_or_default()
    }
}

#[derive(Debug)]
struct NoHostnameVerifier {
    inner: Arc<dyn rustls::client::danger::ServerCertVerifier>,
}

impl rustls::client::danger::ServerCertVerifier for NoHostnameVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Ok(v) => Ok(v),
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName,
            )) => Ok(rustls::client::danger::ServerCertVerified::assertion()),
            Err(e) => Err(e),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
