use super::*;

// ---- Constructor and config method tests ----

#[test]
fn with_webpki_roots_construction_and_alpn() {
    install_crypto_provider();
    let connector = RustlsConnector::with_webpki_roots();
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        "with_webpki_roots should set default ALPN protocols"
    );
}

#[test]
fn with_webpki_roots_versioned_tls13_only() {
    install_crypto_provider();
    let connector = RustlsConnector::with_webpki_roots_versioned(&[&rustls::version::TLS13]);
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        "versioned constructor should still set default ALPN"
    );
}

#[test]
fn with_extra_roots_empty_works_like_webpki_roots() {
    install_crypto_provider();
    let connector = RustlsConnector::with_extra_roots(&[]);
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        "with_extra_roots with empty certs should set default ALPN"
    );
}

#[test]
fn with_extra_roots_versioned_empty_tls13_only() {
    install_crypto_provider();
    let connector = RustlsConnector::with_extra_roots_versioned(&[], &[&rustls::version::TLS13]);
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        "versioned extra roots constructor should still set default ALPN"
    );
}

#[test]
fn config_returns_arc_reference() {
    install_crypto_provider();
    let connector = RustlsConnector::danger_accept_invalid_certs();
    let cfg = connector.config();
    // Verify it returns a reference to the Arc
    assert_eq!(Arc::strong_count(cfg), 1);
    // Clone the connector to increase the strong count
    let connector2 = connector.clone();
    assert_eq!(Arc::strong_count(connector2.config()), 2);
}

#[test]
fn config_mut_clones_on_write_when_shared() {
    install_crypto_provider();
    let connector = RustlsConnector::danger_accept_invalid_certs();
    let connector2 = connector.clone();
    // Both share the same Arc
    assert_eq!(Arc::strong_count(connector.config()), 2);

    // Drop connector2 to reduce count, then take a mutable ref on a shared arc
    let connector_a = connector.clone(); // count = 3
    let mut connector_b = connector.clone(); // count = 4
    let count_before = Arc::strong_count(connector_a.config());
    assert!(count_before > 1, "should be shared before config_mut");

    // config_mut triggers Arc::make_mut which clones because count > 1
    let _cfg_mut = connector_b.config_mut();
    // connector_b now has its own Arc, so connector_a's count decreased
    assert_eq!(
        Arc::strong_count(connector_a.config()),
        count_before - 1,
        "config_mut should clone the Arc when shared"
    );
    drop(connector2);
}

#[cfg(feature = "rustls-native-roots")]
#[test]
fn with_native_roots_construction() {
    install_crypto_provider();
    let connector = RustlsConnector::with_native_roots();
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        "with_native_roots should set default ALPN"
    );
}

#[cfg(feature = "rustls-native-roots")]
#[test]
fn with_native_roots_versioned_tls13_only() {
    install_crypto_provider();
    let connector = RustlsConnector::with_native_roots_versioned(&[&rustls::version::TLS13]);
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        "with_native_roots_versioned should set default ALPN"
    );
}

#[test]
fn danger_accept_invalid_certs_construction() {
    install_crypto_provider();
    let connector = RustlsConnector::danger_accept_invalid_certs();
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

// ---- build_configured tests ----

#[test]
fn build_configured_basic_path_no_crls_no_skip() {
    install_crypto_provider();
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        false,
        None,
    )
    .expect("build_configured with empty CRLs and no skip should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

#[test]
fn build_configured_skip_hostname_verification() {
    install_crypto_provider();
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        true,
        None,
    )
    .expect("build_configured with skip_hostname_verification should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

#[test]
fn build_configured_with_identity_none() {
    install_crypto_provider();
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // identity=None with skip=true exercises the NoHostnameVerifier + no_client_auth path
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS13],
        vec![],
        true,
        None,
    )
    .expect("build_configured with identity=None should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}
