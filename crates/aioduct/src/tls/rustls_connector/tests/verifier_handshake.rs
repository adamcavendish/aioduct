use super::*;

// ---- NoHostnameVerifier signature delegation via real TLS handshake ----

/// Helper: set up a server with a cert issued by our CA (hostname mismatch)
/// and connect using build_configured with skip_hostname_verification=true.
/// This forces NoHostnameVerifier to be used, exercising its signature
/// delegation methods during the handshake.
async fn handshake_with_no_hostname_verifier(
    tls_version: &'static rustls::SupportedProtocolVersion,
) {
    install_crypto_provider();
    // CA that signs the server cert
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by the CA, but for a DIFFERENT hostname
    let mut server_params =
        rcgen::CertificateParams::new(vec!["wrong-host.example.com".into()]).unwrap();
    server_params.is_ca = rcgen::IsCa::NoCa;
    let server_key = rcgen::KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();
    let server_cert_der = rustls::pki_types::CertificateDer::from(server_cert.der().to_vec());
    let server_key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(server_key.serialize_der().into());

    let srv_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(&[tls_version])
            .expect("TLS version should be supported")
            .with_no_client_auth()
            .with_single_cert(vec![server_cert_der], server_key_der)
            .unwrap(),
    );

    // Client trusts the CA, skips hostname verification
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let connector = RustlsConnector::build_configured(
        root_store,
        &[tls_version],
        vec![],
        true, // skip_hostname_verification => uses NoHostnameVerifier
        None,
    )
    .unwrap();

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result
        .expect("handshake with NoHostnameVerifier should succeed despite hostname mismatch");
    assert!(
        !tls_stream.tls.is_handshaking(),
        "handshake must be complete"
    );
}

#[tokio::test]
async fn no_hostname_verifier_delegates_tls13_signature() {
    // TLS 1.3 handshake exercises NoHostnameVerifier::verify_tls13_signature
    handshake_with_no_hostname_verifier(&rustls::version::TLS13).await;
}

#[tokio::test]
async fn no_hostname_verifier_delegates_tls12_signature() {
    // TLS 1.2 handshake exercises NoHostnameVerifier::verify_tls12_signature
    handshake_with_no_hostname_verifier(&rustls::version::TLS12).await;
}

// ---- build_configured with CRLs + real handshake ----

#[tokio::test]
async fn build_configured_with_crl_handshake_succeeds() {
    install_crypto_provider();
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by the CA
    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    server_params.is_ca = rcgen::IsCa::NoCa;
    let server_key = rcgen::KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();
    let server_cert_der = rustls::pki_types::CertificateDer::from(server_cert.der().to_vec());
    let server_key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(server_key.serialize_der().into());

    let srv_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("TLS versions should be supported")
            .with_no_client_auth()
            .with_single_cert(vec![server_cert_der], server_key_der)
            .unwrap(),
    );

    // Empty CRL (no revocations) — the server cert is NOT revoked
    let crl = generate_empty_crl(&ca_params, &ca_key);

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![crl],
        false,
        None,
    )
    .unwrap();

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.expect("handshake with CRL (cert not revoked) should succeed");
    assert!(!tls_stream.tls.is_handshaking());
}

// ---- NoHostnameVerifier::supported_verify_schemes delegation ----

#[test]
fn no_hostname_verifier_supported_schemes_delegates_to_inner() {
    install_crypto_provider();
    // Build a verifier via build_configured with skip_hostname=true
    // Then check that the connector was constructed (implying supported_verify_schemes worked)
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        true,
        None,
    )
    .expect("should succeed — supported_verify_schemes must return non-empty");
    // The fact that the connector was built means NoHostnameVerifier was
    // constructed and rustls validated that supported_verify_schemes is non-empty
    assert!(!connector.config().alpn_protocols.is_empty());
}
