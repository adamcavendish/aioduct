use super::super::*;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::time::Duration;

use futures_util::task::ArcWake;
use tokio::sync::Notify;

#[derive(Debug)]
pub(super) enum ScriptedWrite {
    Partial(usize),
    Zero,
    Error(io::ErrorKind),
    Panic,
}

#[derive(Default)]
struct WriteControlState {
    write_budget: Option<usize>,
    budget_generation: u64,
    scripted_reads: VecDeque<io::ErrorKind>,
    scripted_writes: VecDeque<ScriptedWrite>,
    pending_write_waker: Option<Waker>,
    read_calls: usize,
    write_calls: usize,
    interrupted_flushes_remaining: usize,
    interrupted_shutdowns_remaining: usize,
}

#[derive(Clone, Default)]
pub(super) struct WriteControl {
    state: Arc<Mutex<WriteControlState>>,
    blocked_writes: Arc<AtomicUsize>,
    blocked: Arc<Notify>,
}

impl WriteControl {
    pub(super) fn set_write_budget(&self, write_budget: Option<usize>) {
        let waker = {
            let mut state = self.state.lock().unwrap();
            state.write_budget = write_budget;
            state.budget_generation += 1;
            state.pending_write_waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(super) fn script_writes(&self, writes: impl IntoIterator<Item = ScriptedWrite>) {
        self.state.lock().unwrap().scripted_writes = writes.into_iter().collect();
    }

    pub(super) fn script_reads(&self, reads: impl IntoIterator<Item = io::ErrorKind>) {
        self.state.lock().unwrap().scripted_reads = reads.into_iter().collect();
    }

    pub(super) fn has_pending_write_waker(&self) -> bool {
        self.state.lock().unwrap().pending_write_waker.is_some()
    }

    pub(super) fn write_calls(&self) -> usize {
        self.state.lock().unwrap().write_calls
    }

    pub(super) fn read_calls(&self) -> usize {
        self.state.lock().unwrap().read_calls
    }

    pub(super) fn remaining_scripted_writes(&self) -> usize {
        self.state.lock().unwrap().scripted_writes.len()
    }

    pub(super) fn remaining_scripted_reads(&self) -> usize {
        self.state.lock().unwrap().scripted_reads.len()
    }

    pub(super) fn interrupt_flushes(&self, count: usize) {
        self.state.lock().unwrap().interrupted_flushes_remaining = count;
    }

    pub(super) fn interrupt_shutdowns(&self, count: usize) {
        self.state.lock().unwrap().interrupted_shutdowns_remaining = count;
    }

    fn reset_observations(&self) {
        let mut state = self.state.lock().unwrap();
        state.pending_write_waker = None;
        state.read_calls = 0;
        state.write_calls = 0;
        self.blocked_writes.store(0, Ordering::SeqCst);
    }

    pub(super) async fn wait_for_blocked_writes(&self, expected: usize) {
        loop {
            if self.blocked_writes.load(Ordering::SeqCst) >= expected {
                return;
            }
            self.blocked.notified().await;
        }
    }
}

pub(super) struct WriteBudgetIo<S> {
    inner: S,
    control: WriteControl,
}

impl<S> WriteBudgetIo<S> {
    pub(super) fn new(inner: S, control: WriteControl) -> Self {
        Self { inner, control }
    }
}

impl<S: Read + Unpin> Read for WriteBudgetIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let scripted = {
            let mut state = self.control.state.lock().unwrap();
            state.read_calls += 1;
            state.scripted_reads.pop_front()
        };
        if let Some(kind) = scripted {
            return Poll::Ready(Err(io::Error::from(kind)));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: Write + Unpin> Write for WriteBudgetIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let (scripted, budget, generation) = {
            let mut state = self.control.state.lock().unwrap();
            state.write_calls += 1;
            (
                state.scripted_writes.pop_front(),
                state.write_budget,
                state.budget_generation,
            )
        };

        match scripted {
            Some(ScriptedWrite::Partial(limit)) => {
                assert!(limit > 0, "partial transport writes must make progress");
                let len = limit.min(buf.len());
                return Pin::new(&mut self.inner).poll_write(cx, &buf[..len]);
            }
            Some(ScriptedWrite::Zero) => return Poll::Ready(Ok(0)),
            Some(ScriptedWrite::Error(kind)) => {
                return Poll::Ready(Err(io::Error::from(kind)));
            }
            Some(ScriptedWrite::Panic) => {
                panic!("unexpected second transport write in the same poll")
            }
            None => {}
        }

        if budget == Some(0) {
            self.control.state.lock().unwrap().pending_write_waker = Some(cx.waker().clone());
            self.control.blocked_writes.fetch_add(1, Ordering::SeqCst);
            self.control.blocked.notify_one();
            return Poll::Pending;
        }

        let max_len = budget.unwrap_or(buf.len()).min(buf.len());
        let result = Pin::new(&mut self.inner).poll_write(cx, &buf[..max_len]);
        if let (Some(remaining), Poll::Ready(Ok(n))) = (budget, &result) {
            let mut state = self.control.state.lock().unwrap();
            if state.budget_generation == generation {
                state.write_budget = Some(remaining.saturating_sub(*n));
            }
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        {
            let mut state = self.control.state.lock().unwrap();
            if state.interrupted_flushes_remaining > 0 {
                state.interrupted_flushes_remaining -= 1;
                return Poll::Ready(Err(io::ErrorKind::Interrupted.into()));
            }
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        {
            let mut state = self.control.state.lock().unwrap();
            if state.interrupted_shutdowns_remaining > 0 {
                state.interrupted_shutdowns_remaining -= 1;
                return Poll::Ready(Err(io::ErrorKind::Interrupted.into()));
            }
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub(super) type BudgetTlsStream = TlsStream<WriteBudgetIo<TokioIo<tokio::io::DuplexStream>>>;
pub(super) type TokioServerTlsStream =
    TokioIo<tokio_rustls::server::TlsStream<tokio::io::DuplexStream>>;

pub(super) async fn connected_budget_client() -> (
    BudgetTlsStream,
    TokioIo<tokio::io::DuplexStream>,
    rustls::ServerConnection,
    WriteControl,
) {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let srv_cfg = server_config(certs, key);

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let mut server_stream = TokioIo::new(server_io);
    let connector = RustlsConnector::danger_accept_invalid_certs();
    let control = WriteControl::default();
    let client_io = WriteBudgetIo::new(TokioIo::new(client_io), control.clone());

    let (client_result, srv_conn) = tokio::join!(
        client_connect(&connector, client_io),
        do_server_handshake(srv_cfg, &mut server_stream),
    );
    control.reset_observations();

    (client_result.unwrap(), server_stream, srv_conn, control)
}

pub(super) async fn connected_budget_client_with_h2_server()
-> (BudgetTlsStream, TokioServerTlsStream, WriteControl) {
    install_crypto_provider();
    let (certs, key) = self_signed_cert();
    let mut srv_cfg = server_config(certs, key);
    Arc::get_mut(&mut srv_cfg).unwrap().alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(srv_cfg);

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let connector = RustlsConnector::danger_accept_invalid_certs();
    let control = WriteControl::default();
    let client_io = WriteBudgetIo::new(TokioIo::new(client_io), control.clone());

    let (client_result, server_result) = tokio::join!(
        client_connect(&connector, client_io),
        acceptor.accept(server_io),
    );
    control.reset_observations();

    let client_tls = client_result.unwrap();
    assert_eq!(client_tls.tls.alpn_protocol(), Some(b"h2".as_slice()));
    (client_tls, TokioIo::new(server_result.unwrap()), control)
}

pub(super) async fn shutdown_and_read_plaintext(
    client: &mut BudgetTlsStream,
    tls: &mut rustls::ServerConnection,
    stream: &mut TokioIo<tokio::io::DuplexStream>,
) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(2), async {
        std::future::poll_fn(|cx| Pin::new(&mut *client).poll_shutdown(cx)).await?;

        let mut received = Vec::new();
        loop {
            let mut buf = [0u8; 4096];
            let n = server_read(tls, stream, &mut buf).await?;
            if n == 0 {
                return Ok::<_, io::Error>(received);
            }
            received.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .expect("TLS shutdown and server plaintext read should not hang")
    .expect("TLS shutdown and server plaintext read should succeed")
}

#[derive(Default)]
pub(super) struct WakeCounter(pub(super) AtomicUsize);

impl ArcWake for WakeCounter {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.fetch_add(1, Ordering::SeqCst);
    }
}
