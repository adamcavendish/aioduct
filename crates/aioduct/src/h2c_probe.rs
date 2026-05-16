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
        let Ok(map) = self.inner.lock() else {
            return None;
        };
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
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        if map.len() > 64 {
            let ttl = self.ttl;
            map.retain(|_, cap| match cap {
                H2cCapability::SupportsH2c { probed_at } | H2cCapability::H1Only { probed_at } => {
                    probed_at.elapsed() < ttl
                }
            });
        }
        map.insert(
            authority,
            H2cCapability::SupportsH2c {
                probed_at: Instant::now(),
            },
        );
    }

    pub(crate) fn record_h1_only(&self, authority: Authority) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        if map.len() > 64 {
            let ttl = self.ttl;
            map.retain(|_, cap| match cap {
                H2cCapability::SupportsH2c { probed_at } | H2cCapability::H1Only { probed_at } => {
                    probed_at.elapsed() < ttl
                }
            });
        }
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

    #[test]
    fn expired_h1_only_returns_none() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(0));
        cache.record_h1_only(authority("expired.example.com:80"));
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(cache.lookup(&authority("expired.example.com:80")), None);
    }

    #[test]
    fn overwrite_h1_with_h2c() {
        let cache = H2cProbeCache::new();
        cache.record_h1_only(authority("host.com:80"));
        assert_eq!(cache.lookup(&authority("host.com:80")), Some(false));
        cache.record_h2c(authority("host.com:80"));
        assert_eq!(cache.lookup(&authority("host.com:80")), Some(true));
    }

    #[test]
    fn multiple_authorities_independent() {
        let cache = H2cProbeCache::new();
        cache.record_h2c(authority("a.com:80"));
        cache.record_h1_only(authority("b.com:80"));
        assert_eq!(cache.lookup(&authority("a.com:80")), Some(true));
        assert_eq!(cache.lookup(&authority("b.com:80")), Some(false));
        assert_eq!(cache.lookup(&authority("c.com:80")), None);
    }

    #[test]
    fn clone_shares_state() {
        let cache = H2cProbeCache::new();
        let cloned = cache.clone();
        cache.record_h2c(authority("shared.com:80"));
        assert_eq!(cloned.lookup(&authority("shared.com:80")), Some(true));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn poisoned_mutex_lookup_returns_none() {
        let cache = H2cProbeCache::new();
        // Poison the mutex by panicking inside a lock scope
        let cache_clone = cache.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache_clone.inner.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result.is_err());

        // The mutex is now poisoned; lookup should return None (graceful degradation)
        assert_eq!(cache.lookup(&authority("example.com:80")), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn poisoned_mutex_record_h2c_does_not_panic() {
        let cache = H2cProbeCache::new();
        let cache_clone = cache.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache_clone.inner.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result.is_err());

        // record_h2c on a poisoned mutex should silently do nothing
        cache.record_h2c(authority("poisoned.com:80"));
        // Verify it didn't panic — reaching here means success
        assert_eq!(cache.lookup(&authority("poisoned.com:80")), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn poisoned_mutex_record_h1_only_does_not_panic() {
        let cache = H2cProbeCache::new();
        let cache_clone = cache.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache_clone.inner.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result.is_err());

        // record_h1_only on a poisoned mutex should silently do nothing
        cache.record_h1_only(authority("poisoned.com:80"));
        // Verify it didn't panic — reaching here means success
        assert_eq!(cache.lookup(&authority("poisoned.com:80")), None);
    }

    #[test]
    fn evicts_expired_when_over_capacity() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(1));
        // Fill past threshold
        for i in 0..66 {
            cache.record_h2c(authority(&format!("host{i}.com:80")));
        }
        std::thread::sleep(Duration::from_millis(10));
        // All entries are now expired. Next insert triggers eviction.
        cache.record_h2c(authority("new.com:80"));
        let map = cache.inner.lock().unwrap();
        // Only the new entry should remain (all expired ones evicted)
        assert_eq!(map.len(), 1);
    }
}
