use super::super::*;
use super::support::{ScriptedWrite, connected_budget_client};
use crate::tls::rustls_connector::stream::AsyncWriter;

use std::time::Duration;

struct FlushBackpressureIo {
    pending: bool,
}

impl Write for FlushBackpressureIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[test]
fn async_writer_flush_preserves_transport_backpressure() {
    let mut io = FlushBackpressureIo { pending: true };
    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);

    {
        let mut writer = AsyncWriter::new(&mut io, &mut cx);
        let error = StdWrite::flush(&mut writer).expect_err("Pending must map to WouldBlock");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(writer.is_pending());
    }

    io.pending = false;
    let mut writer = AsyncWriter::new(&mut io, &mut cx);
    StdWrite::flush(&mut writer).expect("a ready transport flush should succeed");
    assert!(!writer.is_pending());
}

#[tokio::test]
async fn poll_write_rejects_data_after_shutdown() {
    let (mut client_tls, _server_stream, _srv_conn, _control) = connected_budget_client().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        std::future::poll_fn(|cx| Pin::new(&mut client_tls).poll_shutdown(cx)).await
    })
    .await
    .expect("TLS shutdown should not hang")
    .expect("TLS shutdown should succeed");

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_write(&mut cx, b"after close") {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
        result => panic!("writes after TLS shutdown must fail, got {result:?}"),
    }
    match Pin::new(&mut client_tls).poll_write(&mut cx, &[]) {
        Poll::Ready(Ok(0)) => {}
        result => panic!("empty writes should remain no-ops after shutdown, got {result:?}"),
    }
}

#[tokio::test]
async fn poll_write_rejects_data_while_shutdown_is_pending() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.set_write_budget(Some(0));

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_shutdown(&mut cx) {
        Poll::Pending => {}
        result => panic!("shutdown should wait for transport capacity, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);

    match Pin::new(&mut client_tls).poll_write(&mut cx, b"after close") {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
        result => panic!("writes must fail once shutdown begins, got {result:?}"),
    }
    assert_eq!(
        control.write_calls(),
        1,
        "rejected writes must not touch the transport"
    );
}

#[tokio::test]
async fn poll_flush_propagates_transport_would_block_without_converting_it_to_pending() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    client_tls
        .tls
        .writer()
        .write_all(b"flush pending ciphertext")
        .unwrap();
    control.script_writes([ScriptedWrite::Error(io::ErrorKind::WouldBlock)]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_flush(&mut cx) {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
        result => panic!("a ready WouldBlock error must remain ready, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);
}

#[tokio::test]
async fn poll_flush_errors_on_zero_ciphertext_write() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    client_tls
        .tls
        .writer()
        .write_all(b"flush pending ciphertext")
        .unwrap();
    control.script_writes([ScriptedWrite::Zero, ScriptedWrite::Panic]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_flush(&mut cx) {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::WriteZero => {}
        result => panic!("poll_flush should report WriteZero, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);
}

#[tokio::test]
async fn poll_shutdown_errors_on_zero_ciphertext_write() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.script_writes([ScriptedWrite::Zero, ScriptedWrite::Panic]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_shutdown(&mut cx) {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::WriteZero => {}
        result => panic!("poll_shutdown should report WriteZero, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);
}

#[tokio::test]
async fn poll_flush_retries_repeated_interrupted_operations() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    client_tls
        .tls
        .writer()
        .write_all(b"flush pending ciphertext")
        .unwrap();
    control.script_writes([
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
    ]);
    control.interrupt_flushes(2);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_flush(&mut cx) {
        Poll::Ready(Ok(())) => {}
        result => panic!("poll_flush should retry Interrupted, got {result:?}"),
    }
    assert!(control.write_calls() >= 3);
}

#[tokio::test]
async fn poll_shutdown_retries_repeated_interrupted_operations() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.script_writes([
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
    ]);
    control.interrupt_shutdowns(2);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_shutdown(&mut cx) {
        Poll::Ready(Ok(())) => {}
        result => panic!("poll_shutdown should retry Interrupted, got {result:?}"),
    }
    assert!(control.write_calls() >= 3);
}

#[tokio::test]
async fn poll_flush_bounds_persistent_interrupted_ciphertext_writes() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    client_tls
        .tls
        .writer()
        .write_all(b"flush pending ciphertext")
        .unwrap();
    control.script_writes((0..33).map(|_| ScriptedWrite::Error(io::ErrorKind::Interrupted)));

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    for remaining in [17, 1] {
        match Pin::new(&mut client_tls).poll_flush(&mut cx) {
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
            result => panic!("persistent Interrupted should be surfaced, got {result:?}"),
        }
        assert_eq!(control.remaining_scripted_writes(), remaining);
    }
    match Pin::new(&mut client_tls).poll_flush(&mut cx) {
        Poll::Ready(Ok(())) => {}
        result => panic!("poll_flush should succeed after interruptions stop, got {result:?}"),
    }
    assert_eq!(control.remaining_scripted_writes(), 0);
}

#[tokio::test]
async fn poll_flush_bounds_persistent_direct_flush_interruptions() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.interrupt_flushes(33);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    for _ in 0..2 {
        match Pin::new(&mut client_tls).poll_flush(&mut cx) {
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
            result => {
                panic!("persistent direct flush interruptions should surface, got {result:?}")
            }
        }
    }
    match Pin::new(&mut client_tls).poll_flush(&mut cx) {
        Poll::Ready(Ok(())) => {}
        result => panic!("poll_flush should succeed after interruptions stop, got {result:?}"),
    }
}

#[tokio::test]
async fn poll_shutdown_bounds_persistent_interrupted_shutdowns() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.interrupt_shutdowns(33);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    for _ in 0..2 {
        match Pin::new(&mut client_tls).poll_shutdown(&mut cx) {
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
            result => panic!("persistent shutdown interruptions should surface, got {result:?}"),
        }
    }
    match Pin::new(&mut client_tls).poll_shutdown(&mut cx) {
        Poll::Ready(Ok(())) => {}
        result => panic!("poll_shutdown should succeed after interruptions stop, got {result:?}"),
    }
}
