use super::*;

#[path = "write_backpressure/handshake.rs"]
mod handshake;
#[path = "write_backpressure/hyper_h1.rs"]
mod hyper_h1;
#[path = "write_backpressure/hyper_h2.rs"]
mod hyper_h2;
#[path = "write_backpressure/lifecycle.rs"]
mod lifecycle;
#[path = "write_backpressure/multipart.rs"]
mod multipart;
#[path = "write_backpressure/read.rs"]
mod read;
#[path = "write_backpressure/support.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures_util::task::waker_ref;

use support::{ScriptedWrite, WakeCounter, connected_budget_client, shutdown_and_read_plaintext};

#[tokio::test]
async fn poll_write_waits_for_pending_ciphertext_before_more_plaintext() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    control.set_write_budget(Some(1));

    let first_payload = vec![0xAA; 4096];
    {
        let wake_counter = Arc::new(WakeCounter::default());
        let waker = waker_ref(&wake_counter);
        let mut cx = Context::from_waker(&waker);

        let first_n = match Pin::new(&mut client_tls).poll_write(&mut cx, &first_payload) {
            Poll::Ready(Ok(n)) => n,
            result => panic!("first write should accept plaintext, got {result:?}"),
        };
        assert_eq!(first_n, first_payload.len());
        assert!(client_tls.tls.wants_write());

        match Pin::new(&mut client_tls).poll_write(&mut cx, b"more") {
            Poll::Pending => {}
            result => panic!("new plaintext must wait for pending ciphertext, got {result:?}"),
        }
        assert!(control.has_pending_write_waker());

        let wakes_before = wake_counter.0.load(Ordering::SeqCst);
        control.script_writes([
            ScriptedWrite::Error(io::ErrorKind::Interrupted),
            ScriptedWrite::Error(io::ErrorKind::Interrupted),
        ]);
        control.set_write_budget(None);
        assert!(wake_counter.0.load(Ordering::SeqCst) > wakes_before);

        match Pin::new(&mut client_tls).poll_write(&mut cx, b"more") {
            Poll::Ready(Ok(4)) => {}
            result => {
                panic!("write should resume after transport capacity returns, got {result:?}")
            }
        }
    }

    let received =
        shutdown_and_read_plaintext(&mut client_tls, &mut srv_conn, &mut server_stream).await;
    let mut expected = first_payload;
    expected.extend_from_slice(b"more");
    assert_eq!(received, expected);
}

#[tokio::test]
async fn poll_write_defers_post_drain_write_zero_after_accepted_plaintext() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.script_writes([ScriptedWrite::Zero, ScriptedWrite::Panic]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_write(&mut cx, b"hello") {
        Poll::Ready(Ok(5)) => {}
        result => panic!("accepted plaintext must be reported before WriteZero, got {result:?}"),
    }
    match Pin::new(&mut client_tls).poll_flush(&mut cx) {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::WriteZero => {}
        result => panic!("the deferred WriteZero must surface on flush, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);
    assert_eq!(control.remaining_scripted_writes(), 1);
}

#[tokio::test]
async fn poll_write_propagates_pre_drain_transport_error() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    client_tls
        .tls
        .writer()
        .write_all(b"pending ciphertext")
        .unwrap();
    control.script_writes([
        ScriptedWrite::Error(io::ErrorKind::BrokenPipe),
        ScriptedWrite::Panic,
    ]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_write(&mut cx, b"more") {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
        result => panic!("pending ciphertext errors must precede new plaintext, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);
    assert_eq!(control.remaining_scripted_writes(), 1);
}

#[tokio::test]
async fn poll_write_reports_write_zero_when_plaintext_capacity_is_zero() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    client_tls.tls.set_buffer_limit(Some(0));

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_write(&mut cx, b"blocked") {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::WriteZero => {}
        result => panic!("zero plaintext capacity must report WriteZero, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 0);
}

#[tokio::test]
async fn poll_write_retries_ciphertext_after_post_drain_interrupted() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    control.script_writes([
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
        ScriptedWrite::Error(io::ErrorKind::Interrupted),
    ]);

    {
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match Pin::new(&mut client_tls).poll_write(&mut cx, b"hello") {
            Poll::Ready(Ok(5)) => {}
            result => panic!("accepted plaintext must survive Interrupted, got {result:?}"),
        }
        match Pin::new(&mut client_tls).poll_write(&mut cx, b"more") {
            Poll::Ready(Ok(4)) => {}
            result => {
                panic!("queued ciphertext should be retried before more data, got {result:?}")
            }
        }
    }

    let received =
        shutdown_and_read_plaintext(&mut client_tls, &mut srv_conn, &mut server_stream).await;
    assert_eq!(received, b"hellomore");
}

#[tokio::test]
async fn poll_write_defers_post_drain_transport_error() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.script_writes([
        ScriptedWrite::Error(io::ErrorKind::BrokenPipe),
        ScriptedWrite::Panic,
    ]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_write(&mut cx, b"hello") {
        Poll::Ready(Ok(5)) => {}
        result => panic!("accepted plaintext must be reported before the error, got {result:?}"),
    }
    match Pin::new(&mut client_tls).poll_flush(&mut cx) {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
        result => panic!("the deferred transport error must surface on flush, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);
    assert_eq!(control.remaining_scripted_writes(), 1);
}

#[tokio::test]
async fn poll_write_surfaces_deferred_error_before_accepting_more_plaintext() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    control.script_writes([
        ScriptedWrite::Error(io::ErrorKind::BrokenPipe),
        ScriptedWrite::Panic,
    ]);

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_write(&mut cx, b"accepted") {
        Poll::Ready(Ok(8)) => {}
        result => panic!("accepted plaintext must precede the transport error, got {result:?}"),
    }
    match Pin::new(&mut client_tls).poll_write(&mut cx, b"rejected") {
        Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
        result => panic!("the deferred error must precede more plaintext, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 1);
    assert_eq!(control.remaining_scripted_writes(), 1);
}

#[tokio::test]
async fn poll_write_retries_after_partial_ciphertext_progress_without_duplication() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    control.script_writes([
        ScriptedWrite::Partial(1),
        ScriptedWrite::Error(io::ErrorKind::BrokenPipe),
    ]);

    {
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match Pin::new(&mut client_tls).poll_write(&mut cx, b"hello") {
            Poll::Ready(Ok(5)) => {}
            result => panic!("partial ciphertext progress must preserve the write, got {result:?}"),
        }
        match Pin::new(&mut client_tls).poll_flush(&mut cx) {
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
            result => panic!("the deferred transport error must surface once, got {result:?}"),
        }
        match Pin::new(&mut client_tls).poll_flush(&mut cx) {
            Poll::Ready(Ok(())) => {}
            result => panic!("remaining ciphertext should be retryable, got {result:?}"),
        }
    }

    let received =
        shutdown_and_read_plaintext(&mut client_tls, &mut srv_conn, &mut server_stream).await;
    assert_eq!(received, b"hello");
}

#[tokio::test]
async fn poll_shutdown_surfaces_deferred_error_then_closes_without_duplication() {
    let (mut client_tls, mut server_stream, mut srv_conn, control) =
        connected_budget_client().await;
    control.script_writes([ScriptedWrite::Error(io::ErrorKind::BrokenPipe)]);

    {
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        match Pin::new(&mut client_tls).poll_write(&mut cx, b"hello") {
            Poll::Ready(Ok(5)) => {}
            result => panic!("accepted plaintext must survive the transport error, got {result:?}"),
        }
        match Pin::new(&mut client_tls).poll_shutdown(&mut cx) {
            Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => {}
            result => panic!("shutdown must surface the deferred error first, got {result:?}"),
        }
    }

    let received =
        shutdown_and_read_plaintext(&mut client_tls, &mut srv_conn, &mut server_stream).await;
    assert_eq!(received, b"hello");
}

#[tokio::test]
async fn poll_write_drains_control_record_before_reporting_write_zero() {
    let (mut client_tls, mut server_stream, mut srv_conn, _control) =
        connected_budget_client().await;

    srv_conn.refresh_traffic_keys().unwrap();
    while srv_conn.wants_write() {
        std::future::poll_fn(|cx| srv_write_tls(&mut srv_conn, &mut server_stream, cx))
            .await
            .unwrap();
    }
    std::future::poll_fn(|cx| Pin::new(&mut server_stream).poll_flush(cx))
        .await
        .unwrap();

    {
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut read_storage = [0u8; 1];
        let mut read_buf = rt::ReadBuf::new(&mut read_storage);
        match Pin::new(&mut client_tls).poll_read(&mut cx, read_buf.unfilled()) {
            Poll::Pending => {}
            result => {
                panic!("processing a KeyUpdate should finish at transport Pending, got {result:?}")
            }
        }
        assert!(!client_tls.tls.wants_write());

        client_tls.tls.set_buffer_limit(Some(1));
        match Pin::new(&mut client_tls).poll_write(&mut cx, b"x") {
            Poll::Ready(Ok(1)) => {}
            result => panic!("poll_write should drain the KeyUpdate and retry, got {result:?}"),
        }
    }

    let received =
        shutdown_and_read_plaintext(&mut client_tls, &mut srv_conn, &mut server_stream).await;
    assert_eq!(received, b"x");
}

#[tokio::test]
async fn empty_poll_write_does_not_wait_for_pending_ciphertext() {
    let (mut client_tls, _server_stream, _srv_conn, control) = connected_budget_client().await;
    client_tls.tls.writer().write_all(b"pending").unwrap();
    control.set_write_budget(Some(0));

    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(&mut client_tls).poll_write(&mut cx, &[]) {
        Poll::Ready(Ok(0)) => {}
        result => panic!("empty poll_write must complete immediately, got {result:?}"),
    }
    assert_eq!(control.write_calls(), 0);
}
