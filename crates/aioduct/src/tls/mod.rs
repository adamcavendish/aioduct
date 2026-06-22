#[cfg(feature = "rustls")]
mod rustls_connector;
#[cfg(feature = "rustls")]
pub use rustls_connector::{AlpnProtocol, RustlsConnector, TlsStream};

#[cfg(all(
    feature = "rustls",
    not(any(feature = "rustls-ring", feature = "rustls-aws-lc-rs"))
))]
compile_error!("rustls support requires either the `rustls-ring` or `rustls-aws-lc-rs` feature");

#[cfg(all(
    feature = "rustls",
    feature = "rustls-ring",
    feature = "rustls-aws-lc-rs"
))]
compile_error!(
    "rustls support requires exactly one crypto provider; enable either `rustls-ring` or `rustls-aws-lc-rs`, not both"
);

use std::future::Future;
use std::io;
use std::pin::Pin;

#[cfg(feature = "rustls")]
pub(crate) fn crypto_provider() -> std::sync::Arc<rustls::crypto::CryptoProvider> {
    std::sync::Arc::new(crypto_provider_value())
}

#[cfg(feature = "rustls")]
fn crypto_provider_value() -> rustls::crypto::CryptoProvider {
    #[cfg(feature = "rustls-aws-lc-rs")]
    {
        rustls::crypto::aws_lc_rs::default_provider()
    }

    #[cfg(all(not(feature = "rustls-aws-lc-rs"), feature = "rustls-ring"))]
    {
        rustls::crypto::ring::default_provider()
    }

    #[cfg(not(any(feature = "rustls-aws-lc-rs", feature = "rustls-ring")))]
    {
        unreachable!(
            "rustls support requires either the `rustls-ring` or `rustls-aws-lc-rs` feature"
        )
    }
}

#[cfg(all(test, feature = "rustls"))]
pub(crate) fn install_default_crypto_provider() {
    let _ = crypto_provider_value().install_default();
}

/// TLS protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
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
    ) -> Result<Vec<&'static rustls::SupportedProtocolVersion>, crate::error::Error> {
        let all = [TlsVersion::Tls1_2, TlsVersion::Tls1_3];
        let versions: Vec<_> = all
            .into_iter()
            .filter(|v| {
                if let Some(min) = min
                    && *v < min
                {
                    return false;
                }
                if let Some(max) = max
                    && *v > max
                {
                    return false;
                }
                true
            })
            .map(|v| v.to_rustls())
            .collect();
        if versions.is_empty() {
            return Err(crate::error::Error::Other(
                "no TLS versions match the configured min/max constraints".into(),
            ));
        }
        Ok(versions)
    }
}

/// Information about the TLS connection, available after handshake.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
pub struct TlsInfo {
    peer_certificate: Option<Vec<u8>>,
    tls_version: Option<String>,
    cipher_suite: Option<String>,
}

impl TlsInfo {
    /// DER-encoded peer (server) certificate, if available.
    pub fn peer_certificate(&self) -> Option<&[u8]> {
        self.peer_certificate.as_deref()
    }

    /// Negotiated TLS protocol version (e.g. "TLSv1.3").
    pub fn tls_version(&self) -> Option<&str> {
        self.tls_version.as_deref()
    }

    /// Negotiated cipher suite name (e.g. "TLS13_AES_256_GCM_SHA384").
    pub fn cipher_suite(&self) -> Option<&str> {
        self.cipher_suite.as_deref()
    }
}

#[cfg(feature = "rustls")]
impl TlsInfo {
    pub(crate) fn from_rustls(conn: &rustls::ClientConnection) -> Self {
        let peer_certificate = conn
            .peer_certificates()
            .and_then(|certs| certs.first())
            .map(|c| c.as_ref().to_vec());
        let tls_version = conn.protocol_version().map(|v| format!("{v:?}"));
        let cipher_suite = conn
            .negotiated_cipher_suite()
            .map(|cs| format!("{:?}", cs.suite()));
        Self {
            peer_certificate,
            tls_version,
            cipher_suite,
        }
    }
}

/// Extract DNS Subject Alternative Names from a DER-encoded certificate.
#[cfg(feature = "rustls")]
pub(crate) fn extract_sans_from_der(der: &[u8]) -> std::sync::Arc<[String]> {
    use x509_parser::prelude::*;
    let Ok((_, cert)) = X509Certificate::from_der(der) else {
        return std::sync::Arc::from([]);
    };
    let Some(san_ext) = cert.subject_alternative_name().ok().flatten() else {
        return std::sync::Arc::from([]);
    };
    let sans: Vec<String> = san_ext
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some(dns.to_string()),
            _ => None,
        })
        .collect();
    std::sync::Arc::from(sans)
}

/// Async TLS handshake abstraction.
pub trait TlsConnect<S>: Send + Sync + 'static {
    /// The TLS-wrapped stream type returned after handshake.
    type Stream: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static;

    /// Perform a TLS handshake over the given TCP stream.
    fn connect(
        &self,
        server_name: &str,
        stream: S,
    ) -> Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + Send + '_>>;
}

/// `!Send` variant of [`TlsConnect`] for completion-based runtimes (compio).
///
/// Gated on having a `RuntimeLocal` runtime available. Extend the cfg
/// when additional `!Send` runtimes are added.
#[cfg(all(feature = "rustls", feature = "compio"))]
pub(crate) trait TlsConnectLocal<S>: 'static {
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin + 'static;

    fn connect_local(
        &self,
        server_name: &str,
        stream: S,
    ) -> Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + '_>>;
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
#[derive(Debug)]
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
        let crls = rustls_pemfile::crls(&mut reader).collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(crls.into_iter().map(|der| Self { der }).collect())
    }
}

#[cfg(all(test, feature = "rustls"))]
mod tests {
    use super::*;

    fn install_crypto() {
        install_default_crypto_provider();
    }

    #[test]
    fn filter_versions_tls12_only() {
        let versions = TlsVersion::filter_versions(None, Some(TlsVersion::Tls1_2)).unwrap();
        assert_eq!(versions.len(), 1);
    }

    #[test]
    fn filter_versions_tls13_only() {
        let versions = TlsVersion::filter_versions(Some(TlsVersion::Tls1_3), None).unwrap();
        assert_eq!(versions.len(), 1);
    }

    #[test]
    fn filter_versions_both() {
        let versions = TlsVersion::filter_versions(None, None).unwrap();
        assert_eq!(versions.len(), 2);
    }

    #[test]
    fn filter_versions_exact_range() {
        let versions =
            TlsVersion::filter_versions(Some(TlsVersion::Tls1_2), Some(TlsVersion::Tls1_3))
                .unwrap();
        assert_eq!(versions.len(), 2);
    }

    #[test]
    fn filter_versions_empty_returns_error() {
        let result =
            TlsVersion::filter_versions(Some(TlsVersion::Tls1_3), Some(TlsVersion::Tls1_2));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no TLS versions match"));
    }

    #[test]
    fn to_rustls_tls12() {
        install_crypto();
        let v = TlsVersion::Tls1_2.to_rustls();
        assert_eq!(*v, rustls::version::TLS12);
    }

    #[test]
    fn to_rustls_tls13() {
        install_crypto();
        let v = TlsVersion::Tls1_3.to_rustls();
        assert_eq!(*v, rustls::version::TLS13);
    }

    #[test]
    fn tls_version_ord() {
        assert!(TlsVersion::Tls1_2 < TlsVersion::Tls1_3);
    }

    #[test]
    fn tls_info_no_peer_cert() {
        let info = TlsInfo {
            peer_certificate: None,
            tls_version: None,
            cipher_suite: None,
        };
        assert!(info.peer_certificate().is_none());
        assert!(info.tls_version().is_none());
        assert!(info.cipher_suite().is_none());
    }

    #[test]
    fn tls_info_with_peer_cert() {
        let info = TlsInfo {
            peer_certificate: Some(vec![1, 2, 3]),
            tls_version: Some("TLSv1_3".into()),
            cipher_suite: Some("TLS13_AES_256_GCM_SHA384".into()),
        };
        assert_eq!(info.peer_certificate(), Some(&[1, 2, 3][..]));
        assert_eq!(info.tls_version(), Some("TLSv1_3"));
        assert_eq!(info.cipher_suite(), Some("TLS13_AES_256_GCM_SHA384"));
    }

    #[test]
    fn tls_info_debug() {
        let info = TlsInfo {
            peer_certificate: None,
            tls_version: None,
            cipher_suite: None,
        };
        let dbg = format!("{info:?}");
        assert!(dbg.contains("TlsInfo"));
    }

    #[test]
    fn certificate_from_der() {
        let cert = Certificate::from_der(vec![0x30, 0x00]);
        assert!(!cert.der.is_empty());
    }

    #[test]
    fn certificate_from_pem_valid() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let pem = ca.cert.pem();
        let certs = Certificate::from_pem(pem.as_bytes()).unwrap();
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn certificate_from_pem_empty() {
        let certs = Certificate::from_pem(b"").unwrap();
        assert!(certs.is_empty());
    }

    #[test]
    fn identity_from_pem_valid() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = Identity::from_pem(pem.as_bytes()).unwrap();
        assert!(!id.certs.is_empty());
    }

    #[test]
    fn identity_from_pem_no_key_fails() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let pem = ca.cert.pem();
        let err = Identity::from_pem(pem.as_bytes()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn crl_from_der() {
        let crl = CertificateRevocationList::from_der(vec![0x30, 0x00]);
        assert!(!crl.der.is_empty());
    }

    #[test]
    fn crl_from_pem_empty() {
        let crls = CertificateRevocationList::from_pem(b"").unwrap();
        assert!(crls.is_empty());
    }

    #[test]
    fn extract_sans_from_self_signed_cert() {
        install_crypto();
        let names = vec![
            "example.com".into(),
            "www.example.com".into(),
            "api.example.com".into(),
        ];
        let cert = rcgen::generate_simple_self_signed(names).unwrap();
        let der = cert.cert.der();
        let sans = extract_sans_from_der(der.as_ref());
        assert!(sans.contains(&"example.com".to_string()));
        assert!(sans.contains(&"www.example.com".to_string()));
        assert!(sans.contains(&"api.example.com".to_string()));
        assert_eq!(sans.len(), 3);
    }

    #[test]
    fn extract_sans_from_invalid_der_returns_empty() {
        let sans = extract_sans_from_der(&[0x00, 0x01, 0x02]);
        assert!(sans.is_empty());
    }

    #[test]
    fn extract_sans_from_empty_returns_empty() {
        let sans = extract_sans_from_der(&[]);
        assert!(sans.is_empty());
    }
}
