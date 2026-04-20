use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_with_full_bandwidth() {
        let bw = BandwidthLimiter::new(1000);
        assert_eq!(bw.try_consume(500), 500);
        assert_eq!(bw.try_consume(500), 500);
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
}
