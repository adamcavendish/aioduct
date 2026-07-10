use super::super::*;
use super::support::{ScriptedWrite, connected_budget_client};

#[tokio::test]
async fn poll_read_reports_zero_while_draining_pending_tls_write() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    client_tls
        .tls
        .writer()
        .write_all(b"pending client plaintext")
        .unwrap();
    server_write(&mut srv_conn, &mut server_stream, b"incoming")
        .await
        .unwrap();
    control.script_writes([
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
        ScriptedWrite::Zero,
    ]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut storage = [0u8; 16];
    let mut read_buf = rt::ReadBuf::new(&mut storage);
    match Pin::new(&mut client_tls).poll_read(&mut cx, read_buf.unfilled()) {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::WriteZero => {}
        result => panic!("read-side TLS write zero must fail, got {result:?}"),
    }
    assert!(read_buf.filled().is_empty());
    assert_eq!(control.write_calls(), 3);
}

#[tokio::test]
async fn poll_read_returns_plaintext_while_outbound_ciphertext_is_backpressured() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    client_tls
        .tls
        .writer()
        .write_all(b"pending client plaintext")
        .unwrap();
    server_write(&mut srv_conn, &mut server_stream, b"incoming")
        .await
        .unwrap();
    control.set_write_budget(Some(0));

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut storage = [0u8; 16];
    let mut read_buf = rt::ReadBuf::new(&mut storage);
    match Pin::new(&mut client_tls).poll_read(&mut cx, read_buf.unfilled()) {
        Poll::Ready(Ok(())) => {}
        result => panic!("available plaintext must not wait for outbound capacity, got {result:?}"),
    }
    assert_eq!(read_buf.filled(), b"incoming");
    assert!(client_tls.tls.wants_write());
    assert!(control.has_pending_write_waker());
    assert_eq!(control.write_calls(), 1);
}

#[tokio::test]
async fn poll_read_propagates_transport_would_block_without_converting_it_to_pending() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.script_reads([io::ErrorKind::WouldBlock]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut storage = [0u8; 16];
    let mut read_buf = rt::ReadBuf::new(&mut storage);
    match Pin::new(&mut client_tls).poll_read(&mut cx, read_buf.unfilled()) {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
        result => panic!("a ready WouldBlock error must remain ready, got {result:?}"),
    }
    assert_eq!(control.read_calls(), 1);
}

#[tokio::test]
async fn poll_read_retries_transient_interrupted_operations() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    server_write(&mut srv_conn, &mut server_stream, b"incoming")
        .await
        .unwrap();
    control.script_reads([io::ErrorKind::Interrupted, io::ErrorKind::Interrupted]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut storage = [0u8; 16];
    let mut read_buf = rt::ReadBuf::new(&mut storage);
    match Pin::new(&mut client_tls).poll_read(&mut cx, read_buf.unfilled()) {
        Poll::Ready(Ok(())) => {}
        result => panic!("transient read interruptions should be retried, got {result:?}"),
    }
    assert_eq!(read_buf.filled(), b"incoming");
    assert!(control.read_calls() >= 3);
}

#[tokio::test]
async fn poll_read_bounds_persistent_interrupted_operations() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.script_reads((0..33).map(|_| io::ErrorKind::Interrupted));

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut storage = [0u8; 16];
    let mut read_buf = rt::ReadBuf::new(&mut storage);
    for remaining in [17, 1] {
        match Pin::new(&mut client_tls).poll_read(&mut cx, read_buf.unfilled()) {
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
            result => panic!("persistent Interrupted should be surfaced, got {result:?}"),
        }
        assert_eq!(control.remaining_scripted_reads(), remaining);
    }
    match Pin::new(&mut client_tls).poll_read(&mut cx, read_buf.unfilled()) {
        Poll::Pending => {}
        result => panic!("the real transport Pending should remain Pending, got {result:?}"),
    }
    assert_eq!(control.remaining_scripted_reads(), 0);
    assert!(
        control.read_calls() <= 35,
        "one poll must not loop indefinitely on persistent Interrupted"
    );
}
