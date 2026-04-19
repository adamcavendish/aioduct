#[cfg(feature = "rustls")]
mod rustls_connector;
#[cfg(feature = "rustls")]
pub use rustls_connector::{AlpnProtocol, RustlsConnector, TlsStream};

use std::future::Future;
use std::io;
use std::pin::Pin;

use crate::runtime::Runtime;

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
