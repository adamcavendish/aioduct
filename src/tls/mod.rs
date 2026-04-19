#[cfg(feature = "rustls")]
mod rustls_connector;
#[cfg(feature = "rustls")]
pub use rustls_connector::{AlpnProtocol, RustlsConnector, TlsStream};

use std::future::Future;
use std::io;
use std::pin::Pin;

use crate::runtime::Runtime;

/// TLS protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TlsVersion {
    /// TLS 1.2
    Tls1_2,
    /// TLS 1.3
    Tls1_3,
}

#[cfg(feature = "rustls")]
impl TlsVersion {
    pub(crate) fn to_rustls(self) -> &'static rustls::SupportedProtocolVersion {
        match self {
            TlsVersion::Tls1_2 => &rustls::version::TLS12,
            TlsVersion::Tls1_3 => &rustls::version::TLS13,
        }
    }

    pub(crate) fn filter_versions(
        min: Option<TlsVersion>,
        max: Option<TlsVersion>,
    ) -> Vec<&'static rustls::SupportedProtocolVersion> {
        let all = [TlsVersion::Tls1_2, TlsVersion::Tls1_3];
        all.into_iter()
            .filter(|v| {
                if let Some(min) = min {
                    if *v < min {
                        return false;
                    }
                }
                if let Some(max) = max {
                    if *v > max {
                        return false;
                    }
                }
                true
            })
            .map(|v| v.to_rustls())
            .collect()
    }
}

/// Information about the TLS connection, available after handshake.
#[derive(Debug, Clone)]
pub struct TlsInfo {
    peer_certificate: Option<Vec<u8>>,
}

impl TlsInfo {
    /// DER-encoded peer (server) certificate, if available.
    pub fn peer_certificate(&self) -> Option<&[u8]> {
        self.peer_certificate.as_deref()
    }
}

#[cfg(feature = "rustls")]
impl TlsInfo {
    pub(crate) fn from_rustls(conn: &rustls::ClientConnection) -> Self {
        let peer_certificate = conn
            .peer_certificates()
            .and_then(|certs| certs.first())
            .map(|c| c.as_ref().to_vec());
        Self { peer_certificate }
    }
}

/// Async TLS handshake abstraction.
pub trait TlsConnect<R: Runtime>: Send + Sync + 'static {
    type Stream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;

    fn connect(
        &self,
        server_name: &str,
        stream: R::TcpStream,
    ) -> Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + Send + '_>>;
}

#[cfg(feature = "rustls")]
/// A TLS certificate for use as a trusted root CA.
#[derive(Clone)]
pub struct Certificate {
    pub(crate) der: rustls::pki_types::CertificateDer<'static>,
}

#[cfg(feature = "rustls")]
impl Certificate {
    /// Create a certificate from DER-encoded bytes.
    pub fn from_der(der: Vec<u8>) -> Self {
        Self {
            der: rustls::pki_types::CertificateDer::from(der),
        }
    }

    /// Create one or more certificates from PEM-encoded bytes.
    pub fn from_pem(pem: &[u8]) -> io::Result<Vec<Self>> {
        let mut reader = io::BufReader::new(pem);
        let certs =
            rustls_pemfile::certs(&mut reader).collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(certs.into_iter().map(|der| Self { der }).collect())
    }
}

#[cfg(feature = "rustls")]
/// A client identity (certificate + private key) for mutual TLS.
pub struct Identity {
    pub(crate) certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    pub(crate) key: rustls::pki_types::PrivateKeyDer<'static>,
}

#[cfg(feature = "rustls")]
impl Identity {
    /// Create an identity from PEM-encoded certificate chain and private key.
    pub fn from_pem(pem: &[u8]) -> io::Result<Self> {
        let mut reader = io::BufReader::new(pem);
        let certs =
            rustls_pemfile::certs(&mut reader).collect::<std::result::Result<Vec<_>, _>>()?;
        let mut reader = io::BufReader::new(pem);
        let key = rustls_pemfile::private_key(&mut reader)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "no private key found in PEM")
        })?;
        Ok(Self { certs, key })
    }
}

#[cfg(feature = "rustls")]
/// A certificate revocation list (CRL) for revocation checking.
#[derive(Clone)]
pub struct CertificateRevocationList {
    pub(crate) der: rustls::pki_types::CertificateRevocationListDer<'static>,
}

#[cfg(feature = "rustls")]
impl CertificateRevocationList {
    /// Create a CRL from DER-encoded bytes.
    pub fn from_der(der: Vec<u8>) -> Self {
        Self {
            der: rustls::pki_types::CertificateRevocationListDer::from(der),
        }
    }

    /// Create one or more CRLs from PEM-encoded bytes.
    pub fn from_pem(pem: &[u8]) -> io::Result<Vec<Self>> {
        let mut reader = io::BufReader::new(pem);
        let crls =
            rustls_pemfile::crls(&mut reader).collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(crls.into_iter().map(|der| Self { der }).collect())
    }
}
