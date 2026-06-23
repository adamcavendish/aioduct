use super::*;

#[tokio::test]
async fn tls_connect_trait_handshake_completes() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _) = tokio::join!(
        <RustlsConnector as TlsConnect<TokioIo<tokio::io::DuplexStream>>>::connect(
            &connector,
            "localhost",
            TokioIo::new(client_io),
        ),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.expect("TlsConnect::connect should complete handshake");
    assert!(
        !tls_stream.tls.is_handshaking(),
        "handshake must be complete"
    );
}

#[tokio::test]
async fn tls_connect_trait_data_roundtrip() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(16384);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, mut srv_conn) = tokio::join!(
        <RustlsConnector as TlsConnect<TokioIo<tokio::io::DuplexStream>>>::connect(
            &connector,
            "localhost",
            TokioIo::new(client_io),
        ),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    let mut client_tls = client_result.unwrap();

    // Client writes, server reads
    let msg = b"trait connect test";
    let n = std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, msg))
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
    assert_eq!(&buf[..n], msg);
}

#[tokio::test]
async fn tls_connect_trait_eof_returns_error() {
    install_crypto_provider();
    let (client_io, server_io) = tokio::io::duplex(8192);
    drop(server_io);

    let connector = RustlsConnector::danger_accept_invalid_certs();
    let result = <RustlsConnector as TlsConnect<TokioIo<tokio::io::DuplexStream>>>::connect(
        &connector,
        "localhost",
        TokioIo::new(client_io),
    )
    .await;

    assert!(result.is_err(), "connect with dropped peer must fail");
}

#[tokio::test]
async fn tls_connect_trait_invalid_server_name() {
    install_crypto_provider();
    let connector = RustlsConnector::danger_accept_invalid_certs();
    let (client_io, _server_io) = tokio::io::duplex(8192);

    // An empty string is not a valid server name
    let result = <RustlsConnector as TlsConnect<TokioIo<tokio::io::DuplexStream>>>::connect(
        &connector,
        "",
        TokioIo::new(client_io),
    )
    .await;

    assert!(result.is_err(), "empty server name should fail");
    let err = result.err().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}
