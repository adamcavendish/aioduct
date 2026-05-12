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
