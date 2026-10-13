use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};

pin_project_lite::pin_project! {
    /// Counts original DATA before decoding, without clocks or callbacks.
    pub(super) struct OriginalDataBody<B> {
        #[pin]
        inner: B,
        total: u64,
        snapshot: Arc<AtomicU64>,
    }
}

impl<B> OriginalDataBody<B> {
    pub(super) fn new(inner: B, snapshot: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            total: 0,
            snapshot,
        }
    }
}

impl<B: Body<Data = Bytes>> Body for OriginalDataBody<B> {
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = self.project();
        let result = this.inner.poll_frame(cx);
        if let Poll::Ready(Some(Ok(frame))) = &result
            && let Some(data) = frame.data_ref()
        {
            *this.total = this.total.saturating_add(data.len() as u64);
            // Single-writer invariant: Each OriginalDataBody instance exclusively owns
            // write access to its snapshot Arc<AtomicU64>. The snapshot is read exactly
            // once after body completion in RequestObservation::emit_finish().
            //
            // Relaxed ordering is sufficient because:
            // 1. No concurrent writes exist (single body instance per request)
            // 2. The read happens after poll_frame returns Poll::Ready(None), establishing
            //    a happens-before relationship through task scheduling
            // 3. We only need eventual visibility, not synchronization with other atomics
            //
            // Using Relaxed avoids unnecessary memory barriers in the hot path.
            this.snapshot.store(*this.total, Ordering::Relaxed);
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
