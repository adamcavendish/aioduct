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
    const DEFAULT_ALPN: &[&[u8]] = &[b"h2", b"http/1.1"];

    /// Create a connector from a rustls client config.
    pub fn new(config: Arc<rustls::ClientConfig>) -> Self {
        Self { config }
    }

    fn set_default_alpn(config: &mut rustls::ClientConfig) {
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
        let mut config = rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::set_default_alpn(&mut config);
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
            root_store
                .add(cert.der.clone())
                .expect("invalid extra root certificate");
        }
        let mut config = rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::set_default_alpn(&mut config);
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
            root_store.add(cert.der.clone()).map_err(io::Error::other)?;
        }
        let mut config = rustls::ClientConfig::builder_with_protocol_versions(versions)
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
        if native_certs.certs.is_empty() && !native_certs.errors.is_empty() {
            panic!(
                "failed to load any native root certificates: {:?}",
                native_certs.errors
            );
        }
        for cert in native_certs.certs {
            let _ = root_store.add(cert);
        }
        let mut config = rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Self::set_default_alpn(&mut config);
        Self::new(Arc::new(config))
    }

    /// Create a connector that accepts any server certificate (INSECURE — testing only).
    pub fn danger_accept_invalid_certs() -> Self {
        let mut config = rustls::ClientConfig::builder()
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
                rustls::client::WebPkiServerVerifier::builder(Arc::new(root_store));
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

            let config = rustls::ClientConfig::builder_with_protocol_versions(versions)
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
            let builder = rustls::ClientConfig::builder_with_protocol_versions(versions)
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
            let mut tls_stream = TlsStream::new(stream, tls_conn);

            // Drive the TLS handshake to completion before returning.
            // rustls queues the ClientHello on construction; we must
            // alternate write_tls / read_tls until the handshake is done.
            while tls_stream.tls.is_handshaking() {
                while tls_stream.tls.wants_write() {
                    std::future::poll_fn(|cx| {
                        write_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx)
                    })
                    .await?;
                }
                std::future::poll_fn(|cx| Pin::new(&mut tls_stream.inner).poll_flush(cx)).await?;
                if tls_stream.tls.wants_read() {
                    let n = std::future::poll_fn(|cx| {
                        read_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx)
                    })
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
                } else if !tls_stream.tls.wants_write() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "TLS handshake stalled: neither wants_read nor wants_write",
                    ));
                }
            }

            Ok(tls_stream)
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

        let plaintext_slice = unsafe {
            let uninit = buf.as_mut();
            std::slice::from_raw_parts_mut(uninit.as_mut_ptr() as *mut u8, uninit.len())
        };

        // First, try to read any buffered plaintext from rustls
        match this.tls.reader().read(plaintext_slice) {
            Ok(n) if n > 0 => {
                unsafe { buf.advance(n) };
                return Poll::Ready(Ok(()));
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Poll::Ready(Err(e)),
        }

        // Keep feeding ciphertext until we get plaintext or the inner stream
        // returns Pending. This handles TLS messages (like NewSessionTicket)
        // that produce no plaintext — we must loop back to read_tls so the
        // waker gets properly registered on the inner stream.
        loop {
            match read_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Ok(())),
                Poll::Ready(Ok(_n)) => {
                    this.tls
                        .process_new_packets()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    if this.tls.wants_write() {
                        if let Poll::Ready(Err(e)) = write_tls(&mut this.tls, &mut this.inner, cx) {
                            return Poll::Ready(Err(e));
                        }
                    }
                    match this.tls.reader().read(plaintext_slice) {
                        Ok(n) if n > 0 => {
                            unsafe { buf.advance(n) };
                            return Poll::Ready(Ok(()));
                        }
                        Ok(_) => {}
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => return Poll::Ready(Err(e)),
                    }
                    // No plaintext yet (e.g. NewSessionTicket), loop to read more ciphertext
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
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

        // Drain all ciphertext from rustls to the underlying stream
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => break,
            }
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // Drain all remaining ciphertext from rustls to the underlying stream
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        // Also flush the underlying stream
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.tls.send_close_notify();

        // Drain the close_notify
        while this.tls.wants_write() {
            match write_tls(&mut this.tls, &mut this.inner, cx) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
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
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(v) => Ok(v),
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName))
            | Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForNameContext { .. },
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

#[cfg(all(test, feature = "rustls", feature = "tokio"))]
mod tests {
    use super::*;
    use crate::runtime::tokio_rt::TokioIo;

    fn install_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn self_signed_cert() -> (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    ) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
        (vec![cert_der], key_der)
    }

    fn server_config(
        certs: Vec<rustls::pki_types::CertificateDer<'static>>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> Arc<rustls::ServerConfig> {
        Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        )
    }

    fn srv_read_tls<S: Read + Unpin>(
        tls: &mut rustls::ServerConnection,
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

    fn srv_write_tls<S: Write + Unpin>(
        tls: &mut rustls::ServerConnection,
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

    async fn do_server_handshake(
        server_cfg: Arc<rustls::ServerConfig>,
        stream: &mut TokioIo<tokio::io::DuplexStream>,
    ) -> rustls::ServerConnection {
        let mut tls = rustls::ServerConnection::new(server_cfg).unwrap();
        while tls.is_handshaking() {
            if tls.wants_read() {
                let n = std::future::poll_fn(|cx| srv_read_tls(&mut tls, stream, cx))
                    .await
                    .unwrap();
                if n == 0 {
                    panic!("server: unexpected EOF during handshake");
                }
                tls.process_new_packets()
                    .expect("server: process_new_packets failed");
            }
            while tls.wants_write() {
                std::future::poll_fn(|cx| srv_write_tls(&mut tls, stream, cx))
                    .await
                    .unwrap();
            }
            std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_flush(cx))
                .await
                .unwrap();
        }
        // After handshake completes, flush any remaining data (e.g. TLS 1.3
        // NewSessionTicket) so the client can finish its handshake.
        while tls.wants_write() {
            std::future::poll_fn(|cx| srv_write_tls(&mut tls, stream, cx))
                .await
                .unwrap();
        }
        std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_flush(cx))
            .await
            .unwrap();
        tls
    }

    async fn server_read(
        tls: &mut rustls::ServerConnection,
        stream: &mut TokioIo<tokio::io::DuplexStream>,
        out: &mut [u8],
    ) -> io::Result<usize> {
        loop {
            match tls.reader().read(out) {
                Ok(n) if n > 0 => return Ok(n),
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            let n = std::future::poll_fn(|cx| srv_read_tls(tls, stream, cx)).await?;
            if n == 0 {
                return Ok(0);
            }
            tls.process_new_packets()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        }
    }

    async fn server_write(
        tls: &mut rustls::ServerConnection,
        stream: &mut TokioIo<tokio::io::DuplexStream>,
        data: &[u8],
    ) -> io::Result<()> {
        use std::io::Write as _;
        tls.writer().write_all(data)?;
        while tls.wants_write() {
            std::future::poll_fn(|cx| srv_write_tls(tls, stream, cx)).await?;
        }
        std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_flush(cx)).await?;
        Ok(())
    }

    async fn client_connect(
        connector: &RustlsConnector,
        stream: TokioIo<tokio::io::DuplexStream>,
    ) -> io::Result<TlsStream<TokioIo<tokio::io::DuplexStream>>> {
        let config = Arc::clone(connector.config());
        let dns_name = ServerName::try_from("localhost".to_string())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let tls_conn = rustls::ClientConnection::new(config, dns_name).map_err(io::Error::other)?;
        let mut tls_stream = TlsStream::new(stream, tls_conn);

        while tls_stream.tls.is_handshaking() {
            while tls_stream.tls.wants_write() {
                std::future::poll_fn(|cx| {
                    write_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx)
                })
                .await?;
            }
            std::future::poll_fn(|cx| Pin::new(&mut tls_stream.inner).poll_flush(cx)).await?;
            if tls_stream.tls.wants_read() {
                let n = std::future::poll_fn(|cx| {
                    read_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx)
                })
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
        }
        // Flush any remaining handshake data (e.g. TLS 1.3 client Finished)
        while tls_stream.tls.wants_write() {
            std::future::poll_fn(|cx| write_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx))
                .await?;
        }
        std::future::poll_fn(|cx| Pin::new(&mut tls_stream.inner).poll_flush(cx)).await?;
        Ok(tls_stream)
    }

    // ---- Handshake tests ----

    #[tokio::test]
    async fn handshake_completes_tls13() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );

        let tls_stream = client_result.expect("handshake should succeed");
        assert!(
            !tls_stream.tls.is_handshaking(),
            "handshake must be complete before connect() returns"
        );
    }

    #[tokio::test]
    async fn handshake_completes_tls12() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = Arc::new(
            rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS12])
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        );

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );

        let tls_stream = client_result.expect("TLS 1.2 handshake should succeed");
        assert!(!tls_stream.tls.is_handshaking());
    }

    #[tokio::test]
    async fn handshake_eof_returns_error() {
        install_crypto_provider();
        let (client_io, server_io) = tokio::io::duplex(8192);
        drop(server_io);

        let connector = RustlsConnector::danger_accept_invalid_certs();
        let result = client_connect(&connector, TokioIo::new(client_io)).await;

        assert!(result.is_err(), "handshake with dropped peer must fail");
        let err = result.err().unwrap();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
            ),
            "expected EOF or broken pipe, got: {err:?}"
        );
    }

    // ---- Data transfer tests ----

    #[tokio::test]
    async fn write_and_flush_drain_ciphertext() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );
        let mut client_tls = client_result.unwrap();

        let payload = b"hello, world!";
        let n = std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, payload))
            .await
            .expect("write should succeed");
        assert_eq!(n, payload.len());

        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_flush(cx))
            .await
            .expect("flush should succeed");
        assert!(
            !client_tls.tls.wants_write(),
            "no pending ciphertext after flush"
        );
    }

    #[tokio::test]
    async fn shutdown_sends_close_notify() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );
        let mut client_tls = client_result.unwrap();

        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_shutdown(cx))
            .await
            .expect("shutdown should succeed");
        assert!(
            !client_tls.tls.wants_write(),
            "close_notify must be fully drained"
        );
    }

    #[tokio::test]
    async fn read_pends_when_no_data() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );
        let mut client_tls = client_result.unwrap();

        let read_result = tokio::time::timeout(std::time::Duration::from_millis(100), async {
            let mut buf = [0u8; 64];
            let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);
            std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled()))
                .await
        })
        .await;
        assert!(
            read_result.is_err(),
            "read with no data should pend, not return immediately"
        );
    }

    #[tokio::test]
    async fn client_write_server_read_roundtrip() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(16384);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, mut srv_conn) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );
        let mut client_tls = client_result.unwrap();

        let message = b"ping from client";
        let n = std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, message))
            .await
            .unwrap();
        assert_eq!(n, message.len());
        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_flush(cx))
            .await
            .unwrap();

        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            server_read(&mut srv_conn, &mut server_stream, &mut buf),
        )
        .await
        .expect("server read should not timeout")
        .expect("server read should succeed");
        assert_eq!(&buf[..n], message);
    }

    #[tokio::test]
    async fn server_write_client_read_roundtrip() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(16384);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, mut srv_conn) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );
        let mut client_tls = client_result.unwrap();

        let message = b"pong from server";
        server_write(&mut srv_conn, &mut server_stream, message)
            .await
            .unwrap();

        let mut buf = [0u8; 256];
        let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled()))
                .await
        })
        .await
        .expect("client read should not timeout")
        .expect("client read should succeed");

        let n = read_buf.filled().len();
        assert_eq!(&buf[..n], message);
    }

    #[tokio::test]
    async fn bidirectional_echo() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(16384);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, mut srv_conn) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );
        let mut client_tls = client_result.unwrap();

        for i in 0..3u8 {
            let msg = format!("message {i}");

            let n =
                std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, msg.as_bytes()))
                    .await
                    .unwrap();
            assert_eq!(n, msg.len());
            std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_flush(cx))
                .await
                .unwrap();

            let mut buf = [0u8; 256];
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                server_read(&mut srv_conn, &mut server_stream, &mut buf),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(&buf[..n], msg.as_bytes());

            server_write(&mut srv_conn, &mut server_stream, &buf[..n])
                .await
                .unwrap();

            let mut rbuf = [0u8; 256];
            let mut read_buf = hyper::rt::ReadBuf::new(&mut rbuf);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                std::future::poll_fn(|cx| {
                    Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled())
                })
                .await
            })
            .await
            .unwrap()
            .unwrap();

            let rn = read_buf.filled().len();
            assert_eq!(&rbuf[..rn], msg.as_bytes());
        }
    }

    // ---- ALPN negotiation tests ----

    #[tokio::test]
    async fn alpn_h2_negotiated() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let mut srv_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        srv_cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let srv_cfg = Arc::new(srv_cfg);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);

        let mut client_cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"h2".to_vec()];
        let connector = RustlsConnector::new(Arc::new(client_cfg));

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );

        let tls_stream = client_result.unwrap();
        assert_eq!(
            RustlsConnector::negotiated_protocol(&tls_stream.tls),
            Some(AlpnProtocol::H2)
        );
    }

    #[tokio::test]
    async fn alpn_h1_negotiated() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let mut srv_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        srv_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let srv_cfg = Arc::new(srv_cfg);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);

        let mut client_cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let connector = RustlsConnector::new(Arc::new(client_cfg));

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );

        let tls_stream = client_result.unwrap();
        assert_eq!(
            RustlsConnector::negotiated_protocol(&tls_stream.tls),
            Some(AlpnProtocol::H1)
        );
    }

    #[tokio::test]
    async fn alpn_none_when_not_configured() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );

        let tls_stream = client_result.unwrap();
        assert_eq!(RustlsConnector::negotiated_protocol(&tls_stream.tls), None);
    }

    #[tokio::test]
    async fn default_alpn_negotiates_h2() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let mut srv_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        srv_cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let srv_cfg = Arc::new(srv_cfg);

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);
        // Uses default ALPN from danger_accept_invalid_certs — no manual config
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );

        let tls_stream = client_result.unwrap();
        assert_eq!(
            RustlsConnector::negotiated_protocol(&tls_stream.tls),
            Some(AlpnProtocol::H2),
        );
    }

    #[test]
    fn default_alpn_set_on_all_constructors() {
        install_crypto_provider();
        let c = RustlsConnector::danger_accept_invalid_certs();
        assert_eq!(
            c.config().alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );

        let c = RustlsConnector::with_webpki_roots();
        assert_eq!(
            c.config().alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    // ---- Large payload (exercises the while-wants_write drain loops) ----

    #[tokio::test]
    async fn large_payload_roundtrip() {
        install_crypto_provider();
        let (certs, key) = self_signed_cert();
        let srv_cfg = server_config(certs, key);

        // Small duplex buffer forces multiple write drain iterations
        let (client_io, server_io) = tokio::io::duplex(4096);
        let mut server_stream = TokioIo::new(server_io);
        let connector = RustlsConnector::danger_accept_invalid_certs();

        let (client_result, mut srv_conn) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );
        let mut client_tls = client_result.unwrap();

        let payload: Vec<u8> = (0..32768).map(|i| (i % 251) as u8).collect();

        let (_, received) = tokio::join!(
            async {
                let mut offset = 0;
                while offset < payload.len() {
                    let n = std::future::poll_fn(|cx| {
                        Pin::new(&mut client_tls).poll_write(cx, &payload[offset..])
                    })
                    .await
                    .unwrap();
                    offset += n;
                }
                std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_flush(cx))
                    .await
                    .unwrap();
            },
            async {
                let mut received = Vec::new();
                while received.len() < payload.len() {
                    let mut buf = [0u8; 4096];
                    let n = server_read(&mut srv_conn, &mut server_stream, &mut buf)
                        .await
                        .unwrap();
                    if n == 0 {
                        break;
                    }
                    received.extend_from_slice(&buf[..n]);
                }
                received
            },
        );

        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);
    }

    // ---- Hostname verification bypass tests ----

    #[tokio::test]
    async fn skip_hostname_verification_allows_mismatched_cert() {
        install_crypto_provider();
        let cert =
            rcgen::generate_simple_self_signed(vec!["wrong-host.example.com".into()]).unwrap();
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());

        let srv_cfg = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der.clone()], key_der)
                .unwrap(),
        );

        let mut root_store = rustls::RootCertStore::empty();
        root_store.add(cert_der).unwrap();

        let connector = RustlsConnector::build_configured(
            root_store,
            &[&rustls::version::TLS12, &rustls::version::TLS13],
            vec![],
            true,
            None,
        )
        .unwrap();

        let (client_io, server_io) = tokio::io::duplex(8192);
        let mut server_stream = TokioIo::new(server_io);

        let (client_result, _) = tokio::join!(
            client_connect(&connector, TokioIo::new(client_io)),
            do_server_handshake(srv_cfg, &mut server_stream),
        );

        assert!(
            client_result.is_ok(),
            "hostname verification skip should allow mismatched cert"
        );
    }
}
