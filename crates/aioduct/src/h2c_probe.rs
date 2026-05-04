use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::uri::Authority;

const DEFAULT_PROBE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
enum H2cCapability {
    SupportsH2c { probed_at: Instant },
    H1Only { probed_at: Instant },
}

/// Caches per-authority h2c capability probe results.
///
/// When using adaptive h2c, the first request to an unknown host attempts an
/// HTTP/2 prior-knowledge handshake. If it succeeds, the host is cached as h2c.
/// If it fails, the host is cached as h1-only. Subsequent requests skip the probe.
#[derive(Clone)]
pub(crate) struct H2cProbeCache {
    inner: Arc<Mutex<HashMap<Authority, H2cCapability>>>,
    ttl: Duration,
}

impl H2cProbeCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl: DEFAULT_PROBE_TTL,
        }
    }

    pub(crate) fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    /// Returns `Some(true)` if h2c is known, `Some(false)` if h1-only, `None` if unknown/expired.
    pub(crate) fn lookup(&self, authority: &Authority) -> Option<bool> {
        let map = self.inner.lock().unwrap();
        match map.get(authority)? {
            H2cCapability::SupportsH2c { probed_at } => {
                if probed_at.elapsed() < self.ttl {
                    Some(true)
                } else {
                    None
                }
            }
            H2cCapability::H1Only { probed_at } => {
                if probed_at.elapsed() < self.ttl {
                    Some(false)
                } else {
                    None
                }
            }
        }
    }

    pub(crate) fn record_h2c(&self, authority: Authority) {
        let mut map = self.inner.lock().unwrap();
        map.insert(
            authority,
            H2cCapability::SupportsH2c {
                probed_at: Instant::now(),
            },
        );
    }

    pub(crate) fn record_h1_only(&self, authority: Authority) {
        let mut map = self.inner.lock().unwrap();
        map.insert(
            authority,
            H2cCapability::H1Only {
                probed_at: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(s: &str) -> Authority {
        s.parse().unwrap()
    }

    #[test]
    fn unknown_returns_none() {
        let cache = H2cProbeCache::new();
        assert_eq!(cache.lookup(&authority("example.com:80")), None);
    }

    #[test]
    fn record_h2c_returns_true() {
        let cache = H2cProbeCache::new();
        cache.record_h2c(authority("grpc.example.com:50051"));
        assert_eq!(
            cache.lookup(&authority("grpc.example.com:50051")),
            Some(true)
        );
    }

    #[test]
    fn record_h1_only_returns_false() {
        let cache = H2cProbeCache::new();
        cache.record_h1_only(authority("legacy.example.com:80"));
        assert_eq!(
            cache.lookup(&authority("legacy.example.com:80")),
            Some(false)
        );
    }

    #[test]
    fn expired_entry_returns_none() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(0));
        cache.record_h2c(authority("expired.example.com:80"));
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(cache.lookup(&authority("expired.example.com:80")), None);
    }
}
