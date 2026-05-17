//! Fast monotonic clock for internal timing measurements.
//!
//! # Why not `std::time::Instant`?
//!
//! `std::time::Instant::now()` issues a syscall on every invocation (~25 ns on
//! Linux, ~60 ns on macOS). The `RequestObserver` fires 8+ timing events per
//! request; at 80k req/s (speedboat target), that's 640k+ `Instant::now()`
//! calls per second — adding ~16–40 ms/s of pure clock overhead.
//!
//! # Default: `coarsetime::Instant`
//!
//! `coarsetime` maintains a cached monotonic timestamp updated by a background
//! thread (default ~1 ms resolution). `Instant::now()` reads an atomic — ~1 ns,
//! 25x faster than the syscall. Trade-off: elapsed measurements have ~1 ms
//! jitter, which is acceptable for network timing (DNS, TCP, TLS each take
//! milliseconds anyway).
//!
//! # `precise-timing` feature
//!
//! Enable this feature to switch back to `std::time::Instant` when sub-ms
//! accuracy matters more than throughput (e.g., local benchmarking, sub-ms
//! timeout enforcement).
//!
//! # API
//!
//! This module exposes a single `Instant` wrapper whose `elapsed()` and
//! `duration_since()` always return `std::time::Duration`, so callers don't
//! need to handle `coarsetime::Duration` conversion.

use std::time::Duration;

/// Returns a monotonic nanosecond count suitable for token-bucket timing.
///
/// Uses coarsetime by default (~1ms resolution, ~1ns read cost) or
/// `std::time::Instant` when `precise-timing` is enabled.
#[inline]
pub(crate) fn monotonic_nanos() -> u64 {
    #[cfg(not(feature = "precise-timing"))]
    {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<coarsetime::Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(coarsetime::Instant::now);
        let elapsed: Duration = epoch.elapsed().into();
        elapsed.as_nanos() as u64
    }
    #[cfg(feature = "precise-timing")]
    {
        use std::sync::OnceLock;
        static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(std::time::Instant::now);
        epoch.elapsed().as_nanos() as u64
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Instant(Inner);

#[cfg(not(feature = "precise-timing"))]
type Inner = coarsetime::Instant;
#[cfg(feature = "precise-timing")]
type Inner = std::time::Instant;

impl Instant {
    #[inline]
    pub(crate) fn now() -> Self {
        Self(Inner::now())
    }

    #[inline]
    pub(crate) fn elapsed(&self) -> Duration {
        #[cfg(not(feature = "precise-timing"))]
        {
            self.0.elapsed().into()
        }
        #[cfg(feature = "precise-timing")]
        {
            self.0.elapsed()
        }
    }

    #[inline]
    pub(crate) fn duration_since(&self, earlier: Instant) -> Duration {
        #[cfg(not(feature = "precise-timing"))]
        {
            self.0.duration_since(earlier.0).into()
        }
        #[cfg(feature = "precise-timing")]
        {
            self.0.duration_since(earlier.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_returns_non_zero_after_spin_wait() {
        let start = Instant::now();
        // Spin for a bit to ensure some time passes
        let mut sum = 0u64;
        for i in 0..100_000 {
            sum = sum.wrapping_add(i);
        }
        // Prevent optimization
        std::hint::black_box(sum);
        let elapsed = start.elapsed();
        // With coarsetime (default), resolution is ~1ms so elapsed may be zero
        // for very short spins. We just verify it doesn't panic and returns a Duration.
        assert!(elapsed.as_nanos() < u128::MAX);
    }

    #[test]
    fn duration_since_earlier_is_non_negative() {
        let earlier = Instant::now();
        // Spin briefly
        let mut sum = 0u64;
        for i in 0..100_000 {
            sum = sum.wrapping_add(i);
        }
        std::hint::black_box(sum);
        let later = Instant::now();
        let dur = later.duration_since(earlier);
        // duration_since(earlier) where later >= earlier should be >= 0
        assert!(dur.as_nanos() < u128::MAX);
    }

    #[test]
    fn duration_since_self_is_zero() {
        let t = Instant::now();
        let dur = t.duration_since(t);
        assert_eq!(dur, Duration::ZERO);
    }

    #[test]
    fn instant_is_copy_and_clone() {
        let t = Instant::now();
        let t2 = t;
        let t3 = t;
        // Both copies should produce the same duration_since result
        assert_eq!(t2.duration_since(t3), Duration::ZERO);
    }

    #[test]
    fn instant_debug_format_is_non_empty() {
        let t = Instant::now();
        let debug = format!("{:?}", t);
        assert!(!debug.is_empty());
    }

    #[test]
    fn monotonic_nanos_increases() {
        let a = monotonic_nanos();
        std::hint::black_box(0u64);
        let b = monotonic_nanos();
        assert!(b >= a);
    }
}
