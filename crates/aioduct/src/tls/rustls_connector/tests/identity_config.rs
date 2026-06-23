use super::*;
use crate::{Certificate, Identity};

// ---- build_configured with identity ----

#[test]
fn build_configured_with_identity_basic_path() {
    install_crypto_provider();
    let cert = rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        false,
        Some((vec![cert_der], key_der)),
    )
    .expect("build_configured with identity should succeed on basic path");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

#[test]
fn build_configured_with_identity_and_skip_hostname() {
    install_crypto_provider();
    let cert = rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS13],
        vec![],
        true,
        Some((vec![cert_der], key_der)),
    )
    .expect("build_configured with identity+skip_hostname should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

// ---- with_extra_roots_versioned with actual extra certs ----

#[test]
fn with_extra_roots_versioned_adds_cert() {
    install_crypto_provider();
    let ca = rcgen::generate_simple_self_signed(vec!["test-ca.local".into()]).unwrap();
    let cert = Certificate::from_der(ca.cert.der().to_vec());

    let connector =
        RustlsConnector::with_extra_roots_versioned(&[cert], &[&rustls::version::TLS13]);
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

// ---- with_identity ----

#[test]
fn with_identity_constructs_successfully() {
    install_crypto_provider();
    let ca = rcgen::generate_simple_self_signed(vec!["ca.local".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(ca.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(ca.signing_key.serialize_der().into());

    let identity = Identity {
        certs: vec![cert_der],
        key: key_der,
    };
    let connector =
        RustlsConnector::with_identity(&[], identity).expect("with_identity should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

#[test]
fn with_identity_versioned_tls13_only() {
    install_crypto_provider();
    let ca = rcgen::generate_simple_self_signed(vec!["ca.local".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(ca.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(ca.signing_key.serialize_der().into());

    let identity = Identity {
        certs: vec![cert_der],
        key: key_der,
    };
    let connector =
        RustlsConnector::with_identity_versioned(&[], identity, &[&rustls::version::TLS13])
            .expect("with_identity_versioned should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}
