use super::super::*;
use super::support::{ScriptedWrite, WriteBudgetIo, WriteControl};
use crate::tls::rustls_connector::config::ensure_handshake_wants_read;

use std::time::Duration;

#[cfg(feature = "compio")]
use crate::tls::TlsConnectLocal;

#[test]
fn handshake_read_guard_rejects_stalled_state() {
    ensure_handshake_wants_read(true).expect("read readiness makes progress");

    let error = ensure_handshake_wants_read(false)
        .expect_err("a handshaking connection must want reads after writes drain");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "TLS handshake stalled: neither wants_read nor wants_write"
    );
}

#[tokio::test]
async fn handshake_errors_on_zero_ciphertext_write() {
    install_crypto_provider();
    let (client_io, _peer_io) = tokio::io::duplex(8192);
    let control = WriteControl::default();
    control.script_writes([ScriptedWrite::Zero]);
    let io = WriteBudgetIo::new(TokioIo::new(client_io), control);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let result = tokio::time::timeout(Duration::from_secs(2), connector.connect("localhost", io))
        .await
        .expect("a zero-progress handshake write must not spin");
    match result {
        Err(e) if e.kind() == io::ErrorKind::WriteZero => {}
        _ => panic!("the handshake should report WriteZero"),
    }
}

#[tokio::test]
async fn handshake_retries_interrupted_flushes() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);
    let control = WriteControl::default();
    control.interrupt_flushes(2);
    let io = WriteBudgetIo::new(TokioIo::new(client_io), control);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let (client_result, _) = tokio::join!(
        connector.connect("localhost", io),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    client_result.expect("transient handshake flush interruptions should be retried");
}

#[tokio::test]
async fn handshake_bounds_persistent_interrupted_flushes() {
    install_crypto_provider();
    let (client_io, _peer_io) = tokio::io::duplex(8192);
    let control = WriteControl::default();
    control.interrupt_flushes(16);
    let io = WriteBudgetIo::new(TokioIo::new(client_io), control);
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let result = tokio::time::timeout(Duration::from_secs(2), connector.connect("localhost", io))
        .await
        .expect("persistent handshake flush interruptions must not spin");
    match result {
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
        _ => panic!("the handshake should preserve persistent Interrupted errors"),
    }
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_handshake_resumes_after_write_backpressure() {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let mut server_stream = TokioIo::new(server_io);
    let control = WriteControl::default();
    control.set_write_budget(Some(1));
    control.interrupt_flushes(2);
    let io = WriteBudgetIo::new(TokioIo::new(client_io), control.clone());
    let connector = RustlsConnector::danger_accept_invalid_certs();

    let release_control = control.clone();
    let release = async move {
        tokio::time::timeout(
            Duration::from_secs(2),
            release_control.wait_for_blocked_writes(1),
        )
        .await
        .expect("local TLS handshake should reach transport backpressure");
        release_control.set_write_budget(None);
    };

    let (client_result, _, ()) = tokio::join!(
        connector.connect_local("localhost", io),
        do_server_handshake(srv_cfg, &mut server_stream),
        release,
    );
    let client = client_result.expect("local TLS handshake should resume after backpressure");
    assert!(!client.tls.is_handshaking());
}
