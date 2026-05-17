use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use crate::error::Error;

// ── UpgradedLocal — !Send upgrade path for compio ──────────────────────────────

trait LocalIo: hyper::rt::Read + hyper::rt::Write + Unpin + 'static {}
impl<T: hyper::rt::Read + hyper::rt::Write + Unpin + 'static> LocalIo for T {}

/// A bidirectional IO stream from an HTTP upgrade on the Local (!Send) path.
///
/// Equivalent to [`super::Upgraded`] but for the Local/compio runtime. Obtained by
/// calling `upgrade()` on a `Response<ResponseBoxLocalBody>`.
pub struct UpgradedLocal {
    io: Box<dyn LocalIo>,
    read_buf: bytes::Bytes,
    read_buf_pos: usize,
}

impl UpgradedLocal {
    pub(crate) fn new<T: hyper::rt::Read + hyper::rt::Write + Unpin + 'static>(
        io: T,
        read_buf: bytes::Bytes,
    ) -> Self {
        Self {
            io: Box::new(io),
            read_buf,
            read_buf_pos: 0,
        }
    }
}

impl hyper::rt::Read for UpgradedLocal {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.read_buf_pos < self.read_buf.len() {
            let remaining = &self.read_buf[self.read_buf_pos..];
            let to_copy = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.read_buf_pos += to_copy;
            return Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut *self.io).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for UpgradedLocal {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut *self.io).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.io).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.io).poll_shutdown(cx)
    }
}

impl std::fmt::Debug for UpgradedLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpgradedLocal").finish()
    }
}

// ── Upgrade handle for Local path ──────────────────────────────────────────────

pub(crate) enum UpgradeState {
    Pending,
    Ready(UpgradedLocal),
    Failed,
}

/// Shared handle between the connection driver task and the upgrade consumer.
/// Uses Arc<Mutex<...>> to be Send+Sync so it can live in http Extensions.
///
/// SAFETY: The Local path is single-threaded (compio is thread-per-core).
/// The Arc<Mutex<...>> is only accessed from one thread — the Mutex provides
/// interior mutability, not cross-thread synchronization.
#[derive(Clone)]
pub(crate) struct UpgradeHandleLocal {
    pub(crate) state: Arc<Mutex<UpgradeState>>,
    pub(crate) waker: Arc<Mutex<Option<Waker>>>,
}

// SAFETY: Local path is single-threaded. UpgradeState contains !Send types
// (Box<dyn LocalIo>) but is only ever accessed from one thread.
unsafe impl Send for UpgradeHandleLocal {}
unsafe impl Sync for UpgradeHandleLocal {}

impl UpgradeHandleLocal {
    #[allow(clippy::arc_with_non_send_sync)]
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(UpgradeState::Pending)),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn fulfill(&self, upgraded: UpgradedLocal) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = UpgradeState::Ready(upgraded);
        if let Some(w) = self.waker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            w.wake();
        }
    }

    pub(crate) fn fail(&self) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = UpgradeState::Failed;
        if let Some(w) = self.waker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            w.wake();
        }
    }
}

pub(crate) async fn on_upgrade_local_manual(
    response: &mut http::Response<crate::body::ResponseBoxLocalBody>,
) -> Result<UpgradedLocal, Error> {
    let handle = response
        .extensions_mut()
        .remove::<UpgradeHandleLocal>()
        .ok_or_else(|| Error::Other("no upgrade handle available".into()))?;

    std::future::poll_fn(|cx| {
        let mut state = handle.state.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *state, UpgradeState::Pending) {
            UpgradeState::Ready(upgraded) => Poll::Ready(Ok(upgraded)),
            UpgradeState::Failed => Poll::Ready(Err(Error::Other("upgrade failed".into()))),
            UpgradeState::Pending => {
                *state = UpgradeState::Pending;
                *handle.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    })
    .await
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use std::future::poll_fn;
    use std::pin::Pin;

    struct MockIo {
        read_data: Vec<u8>,
        written: Vec<u8>,
    }

    impl MockIo {
        fn new(read_data: &[u8]) -> Self {
            Self {
                read_data: read_data.to_vec(),
                written: Vec::new(),
            }
        }
    }

    impl hyper::rt::Read for MockIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.read_data.is_empty() {
                return Poll::Ready(Ok(()));
            }
            let to_copy = self.read_data.len().min(buf.remaining());
            let data: Vec<u8> = self.read_data.drain(..to_copy).collect();
            buf.put_slice(&data);
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Write for MockIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.written.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Unpin for MockIo {}

    #[tokio::test]
    async fn upgraded_local_drains_read_buf_first() {
        let buffered = bytes::Bytes::from_static(b"buffered-");
        let io = MockIo::new(b"stream");
        let mut upgraded = UpgradedLocal::new(io, buffered);

        let mut out = [0u8; 64];
        let mut total = 0;

        let n = poll_fn(|cx| {
            let mut hbuf = hyper::rt::ReadBuf::new(&mut out[total..]);
            match hyper::rt::Read::poll_read(Pin::new(&mut upgraded), cx, hbuf.unfilled()) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(hbuf.filled().len())),
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
        .unwrap();
        total += n;

        let n = poll_fn(|cx| {
            let mut hbuf = hyper::rt::ReadBuf::new(&mut out[total..]);
            match hyper::rt::Read::poll_read(Pin::new(&mut upgraded), cx, hbuf.unfilled()) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(hbuf.filled().len())),
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
        .unwrap();
        total += n;

        assert_eq!(&out[..total], b"buffered-stream");
    }

    #[tokio::test]
    async fn upgraded_local_write_delegates() {
        let io = MockIo::new(b"");
        let mut upgraded = UpgradedLocal::new(io, bytes::Bytes::new());

        let n = poll_fn(|cx| hyper::rt::Write::poll_write(Pin::new(&mut upgraded), cx, b"hello"))
            .await
            .unwrap();
        assert_eq!(n, 5);
    }

    #[tokio::test]
    async fn upgrade_handle_fulfill_wakes() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let handle = UpgradeHandleLocal::new();
                let handle_clone = handle.clone();
                let join = tokio::task::spawn_local(async move {
                    poll_fn(|cx| {
                        let mut state = handle_clone.state.lock().unwrap();
                        match std::mem::replace(&mut *state, UpgradeState::Pending) {
                            UpgradeState::Ready(_) => Poll::Ready(true),
                            UpgradeState::Failed => Poll::Ready(false),
                            UpgradeState::Pending => {
                                *handle_clone.waker.lock().unwrap() = Some(cx.waker().clone());
                                Poll::Pending
                            }
                        }
                    })
                    .await
                });

                tokio::task::yield_now().await;
                let io = MockIo::new(b"");
                handle.fulfill(UpgradedLocal::new(io, bytes::Bytes::new()));
                assert!(join.await.unwrap());
            })
            .await;
    }

    #[tokio::test]
    async fn upgrade_handle_fail_returns_error() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let handle = UpgradeHandleLocal::new();
                let handle_clone = handle.clone();
                let join = tokio::task::spawn_local(async move {
                    poll_fn(|cx| {
                        let mut state = handle_clone.state.lock().unwrap();
                        match std::mem::replace(&mut *state, UpgradeState::Pending) {
                            UpgradeState::Ready(_) => Poll::Ready(Ok(())),
                            UpgradeState::Failed => {
                                Poll::Ready(Err(Error::Other("upgrade failed".into())))
                            }
                            UpgradeState::Pending => {
                                *handle_clone.waker.lock().unwrap() = Some(cx.waker().clone());
                                Poll::Pending
                            }
                        }
                    })
                    .await
                });

                tokio::task::yield_now().await;
                handle.fail();
                assert!(join.await.unwrap().is_err());
            })
            .await;
    }

    #[test]
    fn upgraded_local_debug() {
        let io = MockIo::new(b"");
        let upgraded = UpgradedLocal::new(io, bytes::Bytes::new());
        let dbg = format!("{upgraded:?}");
        assert!(dbg.contains("UpgradedLocal"));
    }
}
