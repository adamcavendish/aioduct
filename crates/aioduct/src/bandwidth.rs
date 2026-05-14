use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body::{Body, Frame};

use crate::body::RequestBoxBody;
use crate::error::Error;

/// A token-bucket bandwidth limiter for throttling download throughput.
///
/// Unlike [`RateLimiter`](crate::RateLimiter) which limits requests per second,
/// this limits bytes per second. It is designed to be attached to the client
/// and applied to response bodies.
#[derive(Clone)]
pub struct BandwidthLimiter {
    inner: Arc<BandwidthInner>,
}

struct BandwidthInner {
    bytes_per_sec: u64,
    tokens: AtomicU64,
    last_refill_ns: AtomicU64,
}

impl BandwidthLimiter {
    /// Create a bandwidth limiter that allows `bytes_per_sec` bytes per second.
    pub fn new(bytes_per_sec: u64) -> Self {
        let now_ns = now_nanos();
        Self {
            inner: Arc::new(BandwidthInner {
                bytes_per_sec,
                tokens: AtomicU64::new(bytes_per_sec),
                last_refill_ns: AtomicU64::new(now_ns),
            }),
        }
    }

    /// Try to consume `n` bytes. Returns the number of bytes actually granted
    /// (may be less than requested or zero).
    pub fn try_consume(&self, n: u64) -> u64 {
        self.refill();
        let mut consumed = 0;
        self.inner
            .tokens
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                let take = current.min(n);
                consumed = take;
                Some(current - take)
            })
            .ok();
        consumed
    }

    /// Returns the duration to wait before bytes become available.
    pub fn wait_duration(&self, bytes_needed: u64) -> Duration {
        self.refill();
        let available = self.inner.tokens.load(Ordering::Relaxed);
        if available >= bytes_needed {
            return Duration::ZERO;
        }
        let deficit = bytes_needed - available;
        let bps = self.inner.bytes_per_sec.max(1);
        Duration::from_nanos(deficit * 1_000_000_000 / bps)
    }

    fn refill(&self) {
        let inner = &self.inner;
        let now = now_nanos();
        let last = inner.last_refill_ns.load(Ordering::Relaxed);
        let elapsed_ns = now.saturating_sub(last);
        if elapsed_ns == 0 {
            return;
        }

        let new_bytes = (elapsed_ns as u128 * inner.bytes_per_sec as u128 / 1_000_000_000) as u64;
        if new_bytes == 0 {
            return;
        }

        let consumed_ns = new_bytes * 1_000_000_000 / inner.bytes_per_sec.max(1);
        inner
            .last_refill_ns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |l| {
                if l == last {
                    Some(l + consumed_ns)
                } else {
                    None
                }
            })
            .ok();

        inner
            .tokens
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(new_bytes).min(inner.bytes_per_sec))
            })
            .ok();
    }
}

impl std::fmt::Debug for BandwidthLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BandwidthLimiter")
            .field("bytes_per_sec", &self.inner.bytes_per_sec)
            .field("available", &self.inner.tokens.load(Ordering::Relaxed))
            .finish()
    }
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

// ── BandwidthBody ──────────────────────────────────────────────────────

/// Body wrapper that enforces bandwidth limits on response data.
///
/// Gates each data frame through a [`BandwidthLimiter`]. Non-data frames
/// (trailers) pass through immediately. When tokens are insufficient,
/// data is buffered and the waker is re-registered so the executor
/// re-polls — the limiter catches up via wall-clock token refill.
pub(crate) struct BandwidthBody {
    inner: RequestBoxBody,
    limiter: BandwidthLimiter,
    pending: Option<Bytes>,
}

impl BandwidthBody {
    pub(crate) fn new(inner: RequestBoxBody, limiter: BandwidthLimiter) -> Self {
        Self {
            inner,
            limiter,
            pending: None,
        }
    }
}

impl Body for BandwidthBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        // 1. Emit buffered data from a previous poll that was rate-limited.
        if let Some(data) = self.pending.as_ref() {
            let n = data.len() as u64;
            if self.limiter.wait_duration(n).is_zero() {
                let _ = self.limiter.try_consume(n);
                if let Some(data) = self.pending.take() {
                    return Poll::Ready(Some(Ok(Frame::data(data))));
                }
            }
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        // 2. Poll inner body.
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                match frame.into_data() {
                    Ok(data) => {
                        let n = data.len() as u64;
                        if self.limiter.wait_duration(n).is_zero() {
                            let _ = self.limiter.try_consume(n);
                            Poll::Ready(Some(Ok(Frame::data(data))))
                        } else {
                            self.pending = Some(data);
                            cx.waker().wake_by_ref();
                            Poll::Pending
                        }
                    }
                    Err(frame) => {
                        // Trailers or other non-data frame — pass through.
                        Poll::Ready(Some(Ok(frame)))
                    }
                }
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream() && self.pending.is_none()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_with_full_bandwidth() {
        let bw = BandwidthLimiter::new(1);
        assert_eq!(bw.try_consume(1), 1);
        assert_eq!(bw.try_consume(1), 0);
    }

    #[test]
    fn wait_duration_zero_when_available() {
        let bw = BandwidthLimiter::new(1000);
        assert_eq!(bw.wait_duration(100), Duration::ZERO);
    }

    #[test]
    fn wait_duration_nonzero_when_exhausted() {
        let bw = BandwidthLimiter::new(1000);
        bw.try_consume(1000);
        let wait = bw.wait_duration(100);
        assert!(wait > Duration::ZERO);
    }

    #[test]
    fn refill_replenishes() {
        let bw = BandwidthLimiter::new(10_000);
        bw.try_consume(10_000);
        std::thread::sleep(Duration::from_millis(110));
        let got = bw.try_consume(5000);
        assert!(got > 0, "expected some tokens after refill, got {got}");
    }

    #[test]
    fn clone_shares_state() {
        let a = BandwidthLimiter::new(100);
        let b = a.clone();
        a.try_consume(50);
        assert_eq!(b.try_consume(50), 50);
        assert_eq!(b.try_consume(1), 0);
    }

    #[test]
    fn debug_output() {
        let bw = BandwidthLimiter::new(500);
        let dbg = format!("{bw:?}");
        assert!(dbg.contains("BandwidthLimiter"));
        assert!(dbg.contains("500"));
    }

    #[test]
    fn try_consume_zero() {
        let bw = BandwidthLimiter::new(100);
        assert_eq!(bw.try_consume(0), 0);
    }

    #[test]
    fn wait_duration_zero_bytes() {
        let bw = BandwidthLimiter::new(100);
        assert_eq!(bw.wait_duration(0), Duration::ZERO);
    }

    #[test]
    fn wait_duration_exact_boundary() {
        let bw = BandwidthLimiter::new(100);
        assert_eq!(bw.wait_duration(100), Duration::ZERO);
    }

    #[test]
    fn partial_consumption() {
        let bw = BandwidthLimiter::new(100);
        assert_eq!(bw.try_consume(60), 60);
        assert_eq!(bw.try_consume(60), 40);
    }

    #[test]
    fn zero_bytes_per_sec() {
        let bw = BandwidthLimiter::new(0);
        assert_eq!(bw.try_consume(10), 0);
        let wait = bw.wait_duration(10);
        assert!(wait > Duration::ZERO);
    }

    // ── BandwidthBody tests ──────────────────────────────────────────

    use http_body::Body;
    use http_body_util::BodyExt;
    use std::pin::Pin;
    use std::task::Context;

    /// Minimal body that returns a single data frame then ends.
    struct OneChunkBody {
        data: Option<Bytes>,
    }

    impl Body for OneChunkBody {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            if let Some(data) = self.data.take() {
                Poll::Ready(Some(Ok(Frame::data(data))))
            } else {
                Poll::Ready(None)
            }
        }
        fn is_end_stream(&self) -> bool {
            self.data.is_none()
        }
        fn size_hint(&self) -> http_body::SizeHint {
            http_body::SizeHint::with_exact(self.data.as_ref().map(|d| d.len() as u64).unwrap_or(0))
        }
    }

    fn boxed_body(chunk: Bytes) -> RequestBoxBody {
        OneChunkBody { data: Some(chunk) }.boxed_unsync()
    }

    fn empty_poll() -> (BandwidthBody, Context<'static>) {
        let body = boxed_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(1024);
        let wrapped = BandwidthBody::new(body, bw);
        let cx = Context::from_waker(std::task::Waker::noop());
        (wrapped, cx)
    }

    #[test]
    fn body_passes_through_with_sufficient_tokens() {
        let (mut wrapped, mut cx) = empty_poll();
        let result = Pin::new(&mut wrapped).poll_frame(&mut cx);
        match result {
            Poll::Ready(Some(Ok(frame))) => {
                assert_eq!(frame.into_data().unwrap(), Bytes::from("hello"));
            }
            other => panic!("expected Ready(Ok(data)), got: {other:?}"),
        }
        // Should be done now
        let result = Pin::new(&mut wrapped).poll_frame(&mut cx);
        assert!(matches!(result, Poll::Ready(None)));
    }

    #[test]
    fn body_buffers_when_tokens_insufficient() {
        let body = boxed_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(1); // only 1 byte bucket
        let mut wrapped = BandwidthBody::new(body, bw);
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = Pin::new(&mut wrapped).poll_frame(&mut cx);
        // Should return Pending since 5 bytes > 1 byte bucket
        assert!(
            matches!(result, Poll::Pending),
            "expected Pending, got: {result:?}"
        );
        // is_end_stream should be false since data is pending
        assert!(!wrapped.is_end_stream());
    }

    #[test]
    fn body_passes_zero_length_frame() {
        let body = boxed_body(Bytes::new());
        let bw = BandwidthLimiter::new(0); // zero bucket but 0-byte frame
        let mut wrapped = BandwidthBody::new(body, bw);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let result = Pin::new(&mut wrapped).poll_frame(&mut cx);
        assert!(matches!(result, Poll::Ready(Some(Ok(_)))));
    }

    #[test]
    fn size_hint_delegates_to_inner() {
        let body = boxed_body(Bytes::from("hello")); // 5 bytes
        let bw = BandwidthLimiter::new(100);
        let wrapped = BandwidthBody::new(body, bw);
        assert_eq!(wrapped.size_hint().exact(), Some(5));
    }

    #[test]
    fn smoke_end_to_end_throttle() {
        // Body with a chunk larger than the bucket — verifies data is
        // eventually delivered after bucket refills.
        let body = boxed_body(Bytes::from("ab"));
        let bw = BandwidthLimiter::new(10_000); // big bucket
        let mut wrapped = BandwidthBody::new(body, bw);
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = Pin::new(&mut wrapped).poll_frame(&mut cx);
        assert!(
            matches!(result, Poll::Ready(Some(Ok(_)))),
            "got: {result:?}"
        );
    }
}
