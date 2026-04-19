use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A token-bucket rate limiter for throttling outgoing requests.
///
/// Tokens replenish at a fixed rate. Each request consumes one token.
/// When no tokens are available, the request waits until one is refilled.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateLimiterInner>,
}

struct RateLimiterInner {
    max_tokens: u64,
    refill_interval: Duration,
    tokens: AtomicU64,
    last_refill_ns: AtomicU64,
}

impl RateLimiter {
    /// Create a rate limiter that allows `max_tokens` requests per `per` duration.
    ///
    /// For example, `RateLimiter::new(10, Duration::from_secs(1))` allows 10 requests
    /// per second, refilling one token every 100ms.
    pub fn new(max_tokens: u64, per: Duration) -> Self {
        let refill_interval = if max_tokens > 0 {
            per / max_tokens as u32
        } else {
            per
        };
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self {
            inner: Arc::new(RateLimiterInner {
                max_tokens,
                refill_interval,
                tokens: AtomicU64::new(max_tokens),
                last_refill_ns: AtomicU64::new(now_ns),
            }),
        }
    }

    /// Try to acquire a token without waiting.
    /// Returns `true` if a token was available.
    pub fn try_acquire(&self) -> bool {
        self.refill();
        self.inner
            .tokens
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current > 0 { Some(current - 1) } else { None }
            })
            .is_ok()
    }

    /// Returns the duration to wait before a token becomes available,
    /// or `Duration::ZERO` if one is available now.
    pub fn wait_duration(&self) -> Duration {
        self.refill();
        if self.inner.tokens.load(Ordering::Relaxed) > 0 {
            Duration::ZERO
        } else {
            self.inner.refill_interval
        }
    }

    fn refill(&self) {
        let inner = &self.inner;
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let last = inner.last_refill_ns.load(Ordering::Relaxed);
        let elapsed_ns = now_ns.saturating_sub(last);
        let refill_ns = inner.refill_interval.as_nanos() as u64;
        if refill_ns == 0 {
            return;
        }
        let new_tokens = elapsed_ns / refill_ns;
        if new_tokens > 0 {
            let consumed_ns = new_tokens * refill_ns;
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
                    Some(current.saturating_add(new_tokens).min(inner.max_tokens))
                })
                .ok();
        }
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("max_tokens", &self.inner.max_tokens)
            .field("refill_interval", &self.inner.refill_interval)
            .field("available", &self.inner.tokens.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_with_full_tokens() {
        let limiter = RateLimiter::new(5, Duration::from_secs(1));
        for _ in 0..5 {
            assert!(limiter.try_acquire());
        }
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn try_acquire_returns_false_when_exhausted() {
        let limiter = RateLimiter::new(2, Duration::from_secs(1));
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn zero_tokens_always_denies() {
        let limiter = RateLimiter::new(0, Duration::from_secs(1));
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn wait_duration_zero_when_tokens_available() {
        let limiter = RateLimiter::new(5, Duration::from_secs(1));
        assert_eq!(limiter.wait_duration(), Duration::ZERO);
    }

    #[test]
    fn wait_duration_nonzero_when_exhausted() {
        let limiter = RateLimiter::new(10, Duration::from_secs(1));
        for _ in 0..10 {
            limiter.try_acquire();
        }
        let wait = limiter.wait_duration();
        assert!(wait > Duration::ZERO);
        assert_eq!(wait, Duration::from_millis(100));
    }

    #[test]
    fn refill_replenishes_after_interval() {
        let limiter = RateLimiter::new(1, Duration::from_millis(50));
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());
        std::thread::sleep(Duration::from_millis(60));
        assert!(limiter.try_acquire());
    }

    #[test]
    fn clone_shares_state() {
        let a = RateLimiter::new(2, Duration::from_secs(10));
        let b = a.clone();
        assert!(a.try_acquire());
        assert!(b.try_acquire());
        assert!(!a.try_acquire());
        assert!(!b.try_acquire());
    }

    #[test]
    fn debug_format() {
        let limiter = RateLimiter::new(5, Duration::from_secs(1));
        let dbg = format!("{limiter:?}");
        assert!(dbg.contains("RateLimiter"));
        assert!(dbg.contains("max_tokens"));
        assert!(dbg.contains("5"));
    }
}
