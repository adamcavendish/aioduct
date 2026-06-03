use super::*;

use std::io;
use std::io::{Read as StdRead, Write as StdWrite};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper::rt::{self, Read, Write};
use rustls::pki_types::ServerName;

use crate::runtime::tokio_rt::TokioIo;
use crate::tls::{TlsConnect, crypto_provider};

fn install_crypto_provider() {
    crate::tls::install_default_crypto_provider();
}

fn self_signed_cert() -> (
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    (vec![cert_der], key_der)
}

fn server_config(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Arc<rustls::ServerConfig> {
    Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
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
            std::future::poll_fn(|cx| write_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx))
                .await?;
        }
        std::future::poll_fn(|cx| Pin::new(&mut tls_stream.inner).poll_flush(cx)).await?;
        if tls_stream.tls.wants_read() {
            let n =
                std::future::poll_fn(|cx| read_tls(&mut tls_stream.tls, &mut tls_stream.inner, cx))
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
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(&[&rustls::version::TLS12])
            .expect("configured rustls provider does not support TLS 1.2")
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
    let cert = rcgen::generate_simple_self_signed(vec!["wrong-host.example.com".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());

    let srv_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
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

// ---- negotiated_protocol via handshake ----

#[tokio::test]
async fn negotiated_protocol_h2_via_tls_connection() {
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
    // danger_accept_invalid_certs sets ALPN to [h2, http/1.1]
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.unwrap();
    let protocol = RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
    assert_eq!(protocol, Some(AlpnProtocol::H2));
}

#[tokio::test]
async fn negotiated_protocol_h1_via_tls_connection() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let mut srv_cfg = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    // Server only supports http/1.1
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
    let protocol = RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
    assert_eq!(protocol, Some(AlpnProtocol::H1));
}

#[tokio::test]
async fn negotiated_protocol_unknown_alpn_returns_none() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let mut srv_cfg = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    // Server advertises a non-standard protocol
    srv_cfg.alpn_protocols = vec![b"custom-proto".to_vec()];
    let srv_cfg = Arc::new(srv_cfg);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let mut client_cfg = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();
    client_cfg.alpn_protocols = vec![b"custom-proto".to_vec()];
    let connector = RustlsConnector::new(Arc::new(client_cfg));

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.unwrap();
    let protocol = RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
    assert_eq!(protocol, None, "unknown ALPN should map to None");
}

#[tokio::test]
async fn negotiated_protocol_no_alpn_returns_none() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    // Server with no ALPN configured
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    // Client with no ALPN configured either
    let mut client_cfg = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider does not support the default TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();
    client_cfg.alpn_protocols = vec![];
    let connector = RustlsConnector::new(Arc::new(client_cfg));

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.unwrap();
    let protocol = RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
    assert_eq!(protocol, None, "no ALPN should return None");
}

// ---- AlpnProtocol trait derive tests ----

#[test]
fn alpn_protocol_debug() {
    let h2 = AlpnProtocol::H2;
    let h1 = AlpnProtocol::H1;
    assert_eq!(format!("{:?}", h2), "H2");
    assert_eq!(format!("{:?}", h1), "H1");
}

#[test]
fn alpn_protocol_copy() {
    let original = AlpnProtocol::H2;
    let copied = original;
    assert_eq!(original, copied);
}

#[test]
fn alpn_protocol_partial_eq() {
    assert_eq!(AlpnProtocol::H2, AlpnProtocol::H2);
    assert_eq!(AlpnProtocol::H1, AlpnProtocol::H1);
    assert_ne!(AlpnProtocol::H2, AlpnProtocol::H1);
}

#[cfg(feature = "json")]
#[test]
fn alpn_protocol_json_roundtrip() {
    let h2 = AlpnProtocol::H2;
    let serialized = serde_json::to_string(&h2).expect("serialize H2");
    let deserialized: AlpnProtocol = serde_json::from_str(&serialized).expect("deserialize H2");
    assert_eq!(h2, deserialized);

    let h1 = AlpnProtocol::H1;
    let serialized = serde_json::to_string(&h1).expect("serialize H1");
    let deserialized: AlpnProtocol = serde_json::from_str(&serialized).expect("deserialize H1");
    assert_eq!(h1, deserialized);
}

// ---- TlsConnect trait (boxed future) tests ----
// These exercise the actual trait impl, not the manual client_connect helper.

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
    let cert = super::super::Certificate::from_der(ca.cert.der().to_vec());

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

    let identity = super::super::Identity {
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

    let identity = super::super::Identity {
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

// ---- TlsStream accessors ----

#[tokio::test]
async fn tls_stream_tls_info_returns_peer_cert() {
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

    let tls_stream = client_result.unwrap();
    let info = tls_stream.tls_info();
    assert!(
        info.peer_certificate().is_some(),
        "peer certificate should be available after handshake"
    );
}

// ---- Clean EOF on read (server closes connection) ----

#[tokio::test]
async fn read_returns_eof_when_server_closes() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(8192);
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

    // Server sends close_notify then closes
    srv_conn.send_close_notify();
    while srv_conn.wants_write() {
        std::future::poll_fn(|cx| srv_write_tls(&mut srv_conn, &mut server_stream, cx))
            .await
            .unwrap();
    }
    std::future::poll_fn(|cx| Pin::new(&mut server_stream).poll_flush(cx))
        .await
        .unwrap();
    drop(server_stream);

    // Client should see EOF (0 bytes read)
    let mut buf = [0u8; 64];
    let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled()))
            .await
    })
    .await
    .expect("read should not hang")
    .expect("read should succeed with EOF");
    assert_eq!(read_buf.filled().len(), 0, "should read 0 bytes on EOF");
}

// ---- build_configured with CRLs ----

/// Helper to generate a CA cert + key suitable for issuing CRLs and signing certs.
fn ca_cert_and_key() -> (rcgen::CertificateParams, rcgen::KeyPair, rcgen::Certificate) {
    let mut params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages.push(rcgen::KeyUsagePurpose::CrlSign);
    params.key_usages.push(rcgen::KeyUsagePurpose::KeyCertSign);
    let key_pair = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    (params, key_pair, cert)
}

/// Generate a valid (empty) CRL signed by the given CA.
fn generate_empty_crl(
    ca_params: &rcgen::CertificateParams,
    ca_key: &rcgen::KeyPair,
) -> rustls::pki_types::CertificateRevocationListDer<'static> {
    use time::{Duration, OffsetDateTime};
    let crl_params = rcgen::CertificateRevocationListParams {
        this_update: OffsetDateTime::now_utc(),
        next_update: OffsetDateTime::now_utc() + Duration::days(30),
        crl_number: rcgen::SerialNumber::from(1u64),
        issuing_distribution_point: None,
        revoked_certs: vec![],
        key_identifier_method: rcgen::KeyIdMethod::Sha256,
    };
    let issuer = rcgen::Issuer::from_params(ca_params, ca_key);
    let crl = crl_params.signed_by(&issuer).unwrap();
    rustls::pki_types::CertificateRevocationListDer::from(crl.der().to_vec())
}

#[test]
fn build_configured_with_nonempty_crls_succeeds() {
    install_crypto_provider();
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let crl = generate_empty_crl(&ca_params, &ca_key);

    // This exercises the `!crls.is_empty()` branch in build_configured
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![crl],
        false,
        None,
    )
    .expect("build_configured with a valid CRL should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

#[test]
fn build_configured_with_crls_and_skip_hostname() {
    install_crypto_provider();
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let crl = generate_empty_crl(&ca_params, &ca_key);

    // Both CRLs and skip_hostname_verification active
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS13],
        vec![crl],
        true,
        None,
    )
    .expect("build_configured with CRL + skip_hostname should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

#[test]
fn build_configured_with_crls_and_identity() {
    install_crypto_provider();
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let crl = generate_empty_crl(&ca_params, &ca_key);

    // Generate a client identity
    let client = rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
    let client_cert_der = rustls::pki_types::CertificateDer::from(client.cert.der().to_vec());
    let client_key_der =
        rustls::pki_types::PrivateKeyDer::Pkcs8(client.signing_key.serialize_der().into());

    // CRLs with identity exercises the `Some((certs, key))` match arm inside the CRL branch
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![crl],
        false,
        Some((vec![client_cert_der], client_key_der)),
    )
    .expect("build_configured with CRL + identity should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

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

// ---- set_default_alpn does not overwrite existing ALPN ----

#[test]
fn set_default_alpn_preserves_existing() {
    install_crypto_provider();
    let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("versions should work")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"custom".to_vec()];
    RustlsConnector::set_default_alpn(&mut config);
    assert_eq!(
        config.alpn_protocols,
        vec![b"custom".to_vec()],
        "set_default_alpn should not overwrite existing ALPN"
    );
}

// ---- poll_write error propagation ----

#[tokio::test]
async fn write_error_propagates_from_underlying_stream() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(16384);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _srv_conn) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    let mut client_tls = client_result.unwrap();

    // Drop the server stream to break the underlying transport
    drop(server_stream);
    drop(_srv_conn);

    // Writing should eventually fail because the underlying transport is broken
    // (the TLS layer will buffer initially, then fail on flush/write_tls)
    let mut write_failed = false;
    for _ in 0..100 {
        let payload = vec![0u8; 16384];
        match std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, &payload)).await {
            Err(_) => {
                write_failed = true;
                break;
            }
            Ok(_) => {
                // Try flushing to force the ciphertext out
                if std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_flush(cx))
                    .await
                    .is_err()
                {
                    write_failed = true;
                    break;
                }
            }
        }
    }
    assert!(
        write_failed,
        "writing to a broken transport should eventually fail"
    );
}

// ---- poll_flush error when underlying stream errors ----

#[tokio::test]
async fn flush_error_propagates() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(16384);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _srv_conn) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    let mut client_tls = client_result.unwrap();

    // Write data so there's something to flush
    let _n = std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, b"data"))
        .await
        .unwrap();

    // Drop server to break the pipe
    drop(server_stream);
    drop(_srv_conn);

    // Multiple flushes should eventually hit an error
    let mut flush_failed = false;
    for _ in 0..10 {
        // Write more to queue ciphertext
        let _ =
            std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, &[0u8; 8192])).await;
        if std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_flush(cx))
            .await
            .is_err()
        {
            flush_failed = true;
            break;
        }
    }
    assert!(
        flush_failed,
        "flush to a broken transport should eventually fail"
    );
}

// ---- Read returns data after server sends and closes ----

#[tokio::test]
async fn read_after_server_write_then_close() {
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

    // Server writes data then closes
    server_write(&mut srv_conn, &mut server_stream, b"final-message")
        .await
        .unwrap();
    srv_conn.send_close_notify();
    while srv_conn.wants_write() {
        std::future::poll_fn(|cx| srv_write_tls(&mut srv_conn, &mut server_stream, cx))
            .await
            .unwrap();
    }
    std::future::poll_fn(|cx| Pin::new(&mut server_stream).poll_flush(cx))
        .await
        .unwrap();
    drop(server_stream);

    // Client reads the data
    let mut buf = [0u8; 256];
    let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled()))
            .await
    })
    .await
    .expect("should not timeout")
    .expect("read should succeed");

    let n = read_buf.filled().len();
    assert_eq!(&buf[..n], b"final-message");

    // Next read should return EOF (0 bytes)
    let mut buf2 = [0u8; 64];
    let mut read_buf2 = hyper::rt::ReadBuf::new(&mut buf2);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf2.unfilled()))
            .await
    })
    .await
    .expect("should not timeout")
    .expect("read should succeed with EOF");
    assert_eq!(read_buf2.filled().len(), 0, "second read should be EOF");
}

// ---- poll_write error when TLS session is closed ----

#[tokio::test]
async fn poll_write_errors_after_close_notify_sent() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(16384);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _srv_conn) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    let mut client_tls = client_result.unwrap();

    // Shut down the TLS session (sends close_notify)
    std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_shutdown(cx))
        .await
        .expect("shutdown should succeed");

    // Now trying to write plaintext into a closed TLS session should error.
    // rustls rejects writes after close_notify has been sent.
    let result =
        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_write(cx, b"after close")).await;
    // After close_notify, the writer should return an error
    assert!(result.is_err(), "writing after close_notify should fail");
}

// ---- poll_shutdown error propagation from write_tls ----

#[tokio::test]
async fn poll_shutdown_propagates_write_error() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(16384);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _srv_conn) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    let mut client_tls = client_result.unwrap();

    // Drop the server side to break the underlying transport
    drop(server_stream);
    drop(_srv_conn);

    // Attempt shutdown — send_close_notify queues data but write_tls should
    // fail because the underlying stream is broken
    let result = std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_shutdown(cx)).await;
    assert!(
        result.is_err(),
        "shutdown with broken transport should propagate write error"
    );
}

// ---- poll_read returns error when underlying stream errors ----

#[tokio::test]
async fn poll_read_propagates_read_error() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(16384);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _srv_conn) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    let mut client_tls = client_result.unwrap();

    // Drop server side abruptly (no close_notify)
    drop(server_stream);
    drop(_srv_conn);

    // Reading should eventually return an error or EOF (0 bytes)
    // since the server didn't send a proper close_notify
    let mut buf = [0u8; 64];
    let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled()))
            .await
    })
    .await
    .expect("read should not hang forever");

    // Either an error or EOF (0 bytes read) is acceptable when peer drops abruptly
    match result {
        Ok(()) => {
            assert_eq!(
                read_buf.filled().len(),
                0,
                "abrupt close should yield EOF (0 bytes)"
            );
        }
        Err(e) => {
            // Any I/O error is acceptable (connection reset, unexpected EOF, etc.)
            assert!(
                e.kind() == io::ErrorKind::UnexpectedEof
                    || e.kind() == io::ErrorKind::ConnectionReset
                    || e.kind() == io::ErrorKind::BrokenPipe
                    || e.kind() == io::ErrorKind::InvalidData,
                "unexpected error kind: {:?}",
                e.kind()
            );
        }
    }
}

// ---- NoHostnameVerifier Ok path (cert IS valid for hostname) ----

#[tokio::test]
async fn no_hostname_verifier_passes_valid_cert_through() {
    install_crypto_provider();
    // Generate CA
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by CA, valid for "localhost" (matches what we connect to)
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
            .expect("versions should be supported")
            .with_no_client_auth()
            .with_single_cert(vec![server_cert_der], server_key_der)
            .unwrap(),
    );

    // Client uses NoHostnameVerifier (skip=true) but the cert IS valid for "localhost"
    // This exercises the `Ok(v) => Ok(v)` path in NoHostnameVerifier::verify_server_cert
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        true, // skip_hostname_verification
        None,
    )
    .unwrap();

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream =
        client_result.expect("handshake should succeed when cert is valid for hostname");
    assert!(!tls_stream.tls.is_handshaking());
    // Verify we actually got the peer cert
    assert!(
        tls_stream.tls_info().peer_certificate().is_some(),
        "peer certificate should be present"
    );
}

// ---- NoHostnameVerifier Err path (cert fails for reasons OTHER than hostname) ----

#[tokio::test]
async fn no_hostname_verifier_rejects_untrusted_cert() {
    install_crypto_provider();
    // Generate a self-signed cert (NOT trusted by the root store)
    let untrusted = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let untrusted_cert_der = rustls::pki_types::CertificateDer::from(untrusted.cert.der().to_vec());
    let untrusted_key_der =
        rustls::pki_types::PrivateKeyDer::Pkcs8(untrusted.signing_key.serialize_der().into());

    let srv_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("versions should be supported")
            .with_no_client_auth()
            .with_single_cert(vec![untrusted_cert_der], untrusted_key_der)
            .unwrap(),
    );

    // Client root store with WebPKI roots (will NOT trust the self-signed cert)
    // skip_hostname_verification=true but cert is not trusted at all
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        true, // skip hostname, but cert still untrusted
        None,
    )
    .unwrap();

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    // Use a server handshake that tolerates client disconnection
    let server_fut = async {
        let mut tls = rustls::ServerConnection::new(srv_cfg).unwrap();
        while tls.is_handshaking() {
            if tls.wants_read() {
                let n =
                    std::future::poll_fn(|cx| srv_read_tls(&mut tls, &mut server_stream, cx)).await;
                match n {
                    Ok(0) => return, // client closed — expected
                    Ok(_) => {
                        if tls.process_new_packets().is_err() {
                            return; // TLS error — expected when client rejects
                        }
                    }
                    Err(_) => return, // I/O error — expected
                }
            }
            while tls.wants_write() {
                if std::future::poll_fn(|cx| srv_write_tls(&mut tls, &mut server_stream, cx))
                    .await
                    .is_err()
                {
                    return; // write error — expected when client closes
                }
            }
            let _ = std::future::poll_fn(|cx| Pin::new(&mut server_stream).poll_flush(cx)).await;
        }
    };

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        server_fut,
    );

    // The handshake should FAIL because the cert is not trusted (not a hostname issue)
    // This exercises the `Err(e) => Err(e)` path in NoHostnameVerifier::verify_server_cert
    match client_result {
        Ok(_) => panic!("handshake with untrusted cert should fail even with hostname skip"),
        Err(err) => {
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidData,
                "should be InvalidData from process_new_packets"
            );
        }
    }
}

// ---- TlsConnect with IP address server name ----

#[tokio::test]
async fn tls_connect_with_ip_address_server_name() {
    install_crypto_provider();
    let connector = RustlsConnector::danger_accept_invalid_certs();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    // Use an IP address as the server name
    let (client_result, _) = tokio::join!(
        <RustlsConnector as TlsConnect<TokioIo<tokio::io::DuplexStream>>>::connect(
            &connector,
            "127.0.0.1",
            TokioIo::new(client_io),
        ),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    // Should succeed because danger_accept_invalid_certs skips cert validation
    let tls_stream = client_result.expect("IP address server name should work");
    assert!(!tls_stream.tls.is_handshaking());
}

// ---- Multiple reads needed to get plaintext (exercises the read loop) ----

#[tokio::test]
async fn read_loop_handles_multiple_tls_records() {
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

    // Server sends multiple small messages rapidly
    for i in 0..5u8 {
        let msg = vec![i; 100];
        server_write(&mut srv_conn, &mut server_stream, &msg)
            .await
            .unwrap();
    }

    // Client reads all data — may need multiple read calls or process
    // multiple TLS records in the read loop
    let mut total_read = Vec::new();
    while total_read.len() < 500 {
        let mut buf = [0u8; 1024];
        let mut read_buf = hyper::rt::ReadBuf::new(&mut buf);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_read(cx, read_buf.unfilled()))
                .await
        })
        .await
        .expect("read should not timeout")
        .expect("read should succeed");

        let n = read_buf.filled().len();
        if n == 0 {
            break;
        }
        total_read.extend_from_slice(&buf[..n]);
    }

    assert_eq!(total_read.len(), 500, "should read all 500 bytes");
    // Verify the data pattern
    for i in 0..5u8 {
        let chunk = &total_read[(i as usize) * 100..(i as usize + 1) * 100];
        assert!(
            chunk.iter().all(|&b| b == i),
            "chunk {i} should be all {i}s"
        );
    }
}

// ---- build_configured with CRLs + identity + skip_hostname (all branches) ----

#[test]
fn build_configured_crls_identity_skip_hostname_all_combined() {
    install_crypto_provider();
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let crl = generate_empty_crl(&ca_params, &ca_key);

    let client = rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
    let client_cert_der = rustls::pki_types::CertificateDer::from(client.cert.der().to_vec());
    let client_key_der =
        rustls::pki_types::PrivateKeyDer::Pkcs8(client.signing_key.serialize_der().into());

    // All options enabled: CRLs + skip_hostname + identity
    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS13],
        vec![crl],
        true,
        Some((vec![client_cert_der], client_key_der)),
    )
    .expect("build_configured with all options should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

// ---- with_identity with extra CA certs ----

#[test]
fn with_identity_with_extra_ca_certs() {
    install_crypto_provider();
    let ca = rcgen::generate_simple_self_signed(vec!["ca.local".into()]).unwrap();
    let ca_cert = super::super::Certificate::from_der(ca.cert.der().to_vec());

    let client = rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
    let client_cert_der = rustls::pki_types::CertificateDer::from(client.cert.der().to_vec());
    let client_key_der =
        rustls::pki_types::PrivateKeyDer::Pkcs8(client.signing_key.serialize_der().into());

    let identity = super::super::Identity {
        certs: vec![client_cert_der],
        key: client_key_der,
    };
    let connector = RustlsConnector::with_identity(&[ca_cert], identity)
        .expect("with_identity + extra certs should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

// ---- with_identity_versioned with extra CA certs ----

#[test]
fn with_identity_versioned_with_extra_certs() {
    install_crypto_provider();
    let ca = rcgen::generate_simple_self_signed(vec!["ca.local".into()]).unwrap();
    let ca_cert = super::super::Certificate::from_der(ca.cert.der().to_vec());

    let client = rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
    let client_cert_der = rustls::pki_types::CertificateDer::from(client.cert.der().to_vec());
    let client_key_der =
        rustls::pki_types::PrivateKeyDer::Pkcs8(client.signing_key.serialize_der().into());

    let identity = super::super::Identity {
        certs: vec![client_cert_der],
        key: client_key_der,
    };
    let connector =
        RustlsConnector::with_identity_versioned(&[ca_cert], identity, &[&rustls::version::TLS13])
            .expect("with_identity_versioned + extra certs should succeed");
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    );
}

// ---- TlsConnect with compio (TlsConnectLocal) ----

#[cfg(feature = "compio")]
#[tokio::test]
async fn tls_connect_local_invalid_server_name() {
    install_crypto_provider();
    let connector = RustlsConnector::danger_accept_invalid_certs();
    let (client_io, _server_io) = tokio::io::duplex(8192);

    // TlsConnectLocal with invalid server name
    let result = <RustlsConnector as super::super::TlsConnectLocal<
        TokioIo<tokio::io::DuplexStream>,
    >>::connect_local(&connector, "", TokioIo::new(client_io))
    .await;

    assert!(result.is_err(), "empty server name should fail");
    let err = result.err().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

// ---- TlsStream::new and tls_connection accessor ----

#[tokio::test]
async fn tls_stream_tls_connection_accessor() {
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

    // tls_connection() should return reference to the connection
    let conn = tls_stream.tls_connection();
    assert!(
        !conn.is_handshaking(),
        "connection should not be handshaking"
    );
    // Verify protocol version is either TLS 1.2 or 1.3
    let version = conn.protocol_version();
    assert!(
        version == Some(rustls::ProtocolVersion::TLSv1_3)
            || version == Some(rustls::ProtocolVersion::TLSv1_2),
        "unexpected protocol version: {:?}",
        version
    );
}

// ---- Mutual TLS handshake with client identity via build_configured ----

#[tokio::test]
async fn mutual_tls_handshake_with_client_identity() {
    install_crypto_provider();
    // Generate CA
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by CA
    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    server_params.is_ca = rcgen::IsCa::NoCa;
    let server_key = rcgen::KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();
    let server_cert_der = rustls::pki_types::CertificateDer::from(server_cert.der().to_vec());
    let server_key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(server_key.serialize_der().into());

    // Client cert (self-signed for simplicity, server requires client auth)
    let client_cert_gen = rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
    let client_cert_der =
        rustls::pki_types::CertificateDer::from(client_cert_gen.cert.der().to_vec());
    let client_key_der =
        rustls::pki_types::PrivateKeyDer::Pkcs8(client_cert_gen.signing_key.serialize_der().into());

    // Server config that requires client auth (accepts any cert for simplicity)
    let mut client_root_store = rustls::RootCertStore::empty();
    client_root_store.add(client_cert_der.clone()).unwrap();
    let client_verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(client_root_store),
        crypto_provider(),
    )
    .allow_unauthenticated()
    .build()
    .unwrap();

    let srv_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("versions should be supported")
            .with_client_cert_verifier(client_verifier)
            .with_single_cert(vec![server_cert_der], server_key_der)
            .unwrap(),
    );

    // Client connector with identity
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        false,
        Some((vec![client_cert_der], client_key_der)),
    )
    .unwrap();

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.expect("mutual TLS handshake should succeed with identity");
    assert!(!tls_stream.tls.is_handshaking());
}

// ---- write_tls and read_tls WouldBlock->Pending conversion ----

#[tokio::test]
async fn write_drain_loop_handles_pending_gracefully() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    // Use a very small duplex buffer to force backpressure
    let (client_io, server_io) = tokio::io::duplex(256);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, mut srv_conn) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    let mut client_tls = client_result.unwrap();

    // Write a large payload that will saturate the small buffer,
    // forcing write_tls to return Pending (WouldBlock) in the drain loop.
    // We need the server to read simultaneously to unblock.
    let payload = vec![0xAA; 8192];
    let (write_result, read_result) = tokio::join!(
        async {
            let mut offset = 0;
            while offset < payload.len() {
                let n = std::future::poll_fn(|cx| {
                    Pin::new(&mut client_tls).poll_write(cx, &payload[offset..])
                })
                .await?;
                offset += n;
                std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_flush(cx)).await?;
            }
            Ok::<(), io::Error>(())
        },
        async {
            let mut total = Vec::new();
            while total.len() < payload.len() {
                let mut buf = [0u8; 512];
                let n = server_read(&mut srv_conn, &mut server_stream, &mut buf).await?;
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
            }
            Ok::<Vec<u8>, io::Error>(total)
        },
    );

    write_result.expect("write should succeed despite backpressure");
    let received = read_result.expect("server read should succeed");
    assert_eq!(received.len(), payload.len());
    assert!(
        received.iter().all(|&b| b == 0xAA),
        "all bytes should be 0xAA"
    );
}

// ---- with_extra_roots with actual CA cert that gets used ----

#[tokio::test]
async fn with_extra_roots_trusts_custom_ca() {
    install_crypto_provider();
    // Generate CA
    let (ca_params, ca_key, ca_cert) = ca_cert_and_key();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by the custom CA
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
            .expect("versions should be supported")
            .with_no_client_auth()
            .with_single_cert(vec![server_cert_der], server_key_der)
            .unwrap(),
    );

    // Client uses with_extra_roots to trust the custom CA
    let extra_cert = super::super::Certificate::from_der(ca_cert_der.to_vec());
    let connector = RustlsConnector::with_extra_roots(&[extra_cert]);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.expect("handshake should succeed with custom CA in extra roots");
    assert!(!tls_stream.tls.is_handshaking());
    assert!(tls_stream.tls_info().peer_certificate().is_some());
}

// ---- negotiated_protocol with h1-only server ----

#[tokio::test]
async fn negotiated_protocol_h1_only_server() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    // Server only supports h1
    let mut srv_cfg = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    srv_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let srv_cfg = Arc::new(srv_cfg);

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        do_server_handshake(srv_cfg, &mut server_stream),
    );

    let tls_stream = client_result.expect("handshake should succeed");
    let proto = RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
    assert_eq!(proto, Some(AlpnProtocol::H1));
}

// ---- config_mut test ----

#[test]
fn config_mut_allows_modification() {
    install_crypto_provider();
    let mut connector = RustlsConnector::with_webpki_roots();
    let config = connector.config_mut();
    // Verify we can modify the ALPN protocols
    config.alpn_protocols = vec![b"h2".to_vec()];
    assert_eq!(connector.config().alpn_protocols, vec![b"h2".to_vec()]);
}

// ---- set_default_alpn test ----

#[test]
fn set_default_alpn_does_not_overwrite_existing() {
    install_crypto_provider();
    let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"custom/1.0".to_vec()];
    RustlsConnector::set_default_alpn(&mut config);
    // Should NOT overwrite existing protocols
    assert_eq!(config.alpn_protocols, vec![b"custom/1.0".to_vec()]);
}

#[cfg(feature = "rustls-aws-lc-rs")]
#[test]
fn custom_config_accepts_ech_enabled_rustls_client_config() {
    use rustls::crypto::hpke::Hpke as _;

    install_crypto_provider();

    let hpke = rustls::crypto::aws_lc_rs::hpke::DH_KEM_P256_HKDF_SHA256_AES_128;
    let (placeholder_key, _) = hpke.generate_key_pair().expect("hpke key pair");
    let ech_mode = rustls::client::EchMode::Grease(rustls::client::EchGreaseConfig::new(
        hpke,
        placeholder_key,
    ));
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let mut config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_ech(ech_mode)
        .expect("ech config")
        .with_root_certificates(root_store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let connector = RustlsConnector::new(Arc::new(config));
    assert_eq!(
        connector.config().alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    );
}

// ---- Hostname verification bypass: chain validation intact, hostname mismatch ignored ----

/// Verify that `danger_accept_invalid_hostnames` (i.e.
/// `build_configured` with `skip_hostname_verification: true`) bypasses
/// only the hostname check while keeping certificate chain validation
/// active.
///
/// The server presents a certificate that is trusted (added to the
/// client's root store) but issued for a different hostname. The
/// connector is built with hostname verification disabled. The handshake
/// must succeed because chain validation passes and the hostname mismatch
/// is tolerated.
///
/// This is different from `danger_accept_invalid_certs()` which disables
/// ALL certificate verification (including chain validation).
#[tokio::test]
async fn no_hostname_verifier_allows_wrong_hostname() {
    install_crypto_provider();

    // Generate CA
    let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .key_usages
        .push(rcgen::KeyUsagePurpose::KeyCertSign);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let ca_cert_der = rustls::pki_types::CertificateDer::from(ca_cert.der().to_vec());

    // Server cert signed by the CA but for a hostname DIFFERENT from
    // what the client will connect to.
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
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![server_cert_der], server_key_der)
            .unwrap(),
    );

    // Client trusts the CA (chain validation passes) but skips hostname
    // verification.
    let mut root_store = rustls::RootCertStore::empty();
    root_store.add(ca_cert_der).unwrap();

    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        true, // skip_hostname_verification
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
        .expect("handshake must succeed: chain validation passes, hostname mismatch is tolerated");
    assert!(!tls_stream.tls.is_handshaking());
}

// ---- danger_accept_invalid_hostnames preserves chain validation ----

/// Verifies that hostname-verification bypass (via
/// `build_configured` with `skip_hostname_verification: true`) does NOT
/// disable certificate chain validation.
///
/// The server presents a self-signed cert for "other.example.com" that is
/// NOT in the client's WebPKI root store. Even though hostname verification
/// is skipped, the handshake must fail because the certificate chain is
/// untrusted.
#[tokio::test]
async fn no_hostname_verifier_preserves_chain_validation() {
    install_crypto_provider();

    // Self-signed cert for a hostname different from what we connect to,
    // and NOT signed by any CA in our root store.
    let untrusted = rcgen::generate_simple_self_signed(vec!["other.example.com".into()]).unwrap();
    let untrusted_cert_der = rustls::pki_types::CertificateDer::from(untrusted.cert.der().to_vec());
    let untrusted_key_der =
        rustls::pki_types::PrivateKeyDer::Pkcs8(untrusted.signing_key.serialize_der().into());

    let srv_cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("versions should be supported")
            .with_no_client_auth()
            .with_single_cert(vec![untrusted_cert_der], untrusted_key_der)
            .unwrap(),
    );

    // Client: WebPKI roots + hostname verification skipped.
    // Chain validation is still active — the self-signed cert is not trusted.
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let connector = RustlsConnector::build_configured(
        root_store,
        &[&rustls::version::TLS12, &rustls::version::TLS13],
        vec![],
        true, // skip hostname verification
        None,
    )
    .unwrap();

    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);

    // Use a tolerant server handshake that handles client disconnection
    let server_fut = async {
        let mut tls = rustls::ServerConnection::new(srv_cfg).unwrap();
        while tls.is_handshaking() {
            if tls.wants_read() {
                let n =
                    std::future::poll_fn(|cx| srv_read_tls(&mut tls, &mut server_stream, cx)).await;
                match n {
                    Ok(0) => return,
                    Ok(_) => {
                        if tls.process_new_packets().is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            while tls.wants_write() {
                if std::future::poll_fn(|cx| srv_write_tls(&mut tls, &mut server_stream, cx))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = std::future::poll_fn(|cx| Pin::new(&mut server_stream).poll_flush(cx)).await;
        }
    };

    let (client_result, _) = tokio::join!(
        client_connect(&connector, TokioIo::new(client_io)),
        server_fut,
    );

    match client_result {
        Ok(_) => {
            panic!("handshake must fail: hostname bypass does not disable chain validation");
        }
        Err(err) => {
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidData,
                "chain validation failure should yield InvalidData, got {:?}",
                err.kind()
            );
        }
    }
}
