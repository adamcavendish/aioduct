use super::*;

// ---- ALPN negotiation tests ----

#[tokio::test]
async fn alpn_h2_negotiated() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let mut srv_cfg = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    srv_cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let srv_cfg = Arc::new(srv_cfg);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let mut client_cfg = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
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
    let mut srv_cfg = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    srv_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let srv_cfg = Arc::new(srv_cfg);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let mut client_cfg = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
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
    let mut srv_cfg = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
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
