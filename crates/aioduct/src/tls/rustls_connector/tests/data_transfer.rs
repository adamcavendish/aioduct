use super::*;

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

        let n = std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, msg.as_bytes()))
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
            std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled()))
                .await
        })
        .await
        .unwrap()
        .unwrap();

        let rn = read_buf.filled().len();
        assert_eq!(&rbuf[..rn], msg.as_bytes());
    }
}
