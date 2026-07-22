use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::uri::{Authority, Scheme};

use crate::pool::ProxyRoute;

const DEFAULT_PROBE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
enum H2cCapability {
    SupportsH2c { probed_at: Instant },
    H1Only { probed_at: Instant },
}

/// Caches h2c capability probe results per effective route.
///
/// When using adaptive h2c, the first request to an unknown host attempts an
/// HTTP/2 prior-knowledge handshake. If it succeeds, the host is cached as h2c.
/// If it fails, that route is cached as h1-only. Subsequent requests on the
/// same route skip the probe.
#[derive(Clone)]
pub(crate) struct H2cProbeCache {
    inner: Arc<Mutex<HashMap<H2cProbeKey, H2cCapability>>>,
    ttl: Duration,
}

/// Identity of the route whose adaptive-h2c result was observed.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) struct H2cProbeKey {
    scheme: Scheme,
    host: String,
    port: u16,
    proxy_route: ProxyRoute,
    forced_addr: Option<SocketAddr>,
}

impl H2cProbeKey {
    pub(crate) fn new(
        scheme: Scheme,
        authority: &Authority,
        proxy_route: ProxyRoute,
        forced_addr: Option<SocketAddr>,
    ) -> Self {
        Self {
            port: authority
                .port_u16()
                .unwrap_or(if scheme == Scheme::HTTP { 80 } else { 443 }),
            scheme,
            host: authority.host().to_ascii_lowercase(),
            proxy_route,
            forced_addr,
        }
    }
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
    pub(crate) fn lookup(&self, key: &H2cProbeKey) -> Option<bool> {
        let Ok(map) = self.inner.lock() else {
            return None;
        };
        match map.get(key)? {
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

    pub(crate) fn record_h2c(&self, key: H2cProbeKey) {
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
            key,
            H2cCapability::SupportsH2c {
                probed_at: Instant::now(),
            },
        );
    }

    pub(crate) fn record_h1_only(&self, key: H2cProbeKey) {
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
            key,
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

    fn key(s: &str) -> H2cProbeKey {
        H2cProbeKey::new(Scheme::HTTP, &authority(s), ProxyRoute::DIRECT, None)
    }

    #[test]
    fn unknown_returns_none() {
        let cache = H2cProbeCache::new();
        assert_eq!(cache.lookup(&key("example.com:80")), None);
    }

    #[test]
    fn record_h2c_returns_true() {
        let cache = H2cProbeCache::new();
        let key = key("grpc.example.com:50051");
        cache.record_h2c(key.clone());
        assert_eq!(cache.lookup(&key), Some(true));
    }

    #[test]
    fn record_h1_only_returns_false() {
        let cache = H2cProbeCache::new();
        let key = key("legacy.example.com:80");
        cache.record_h1_only(key.clone());
        assert_eq!(cache.lookup(&key), Some(false));
    }

    #[test]
    fn expired_entry_returns_none() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(0));
        let key = key("expired.example.com:80");
        cache.record_h2c(key.clone());
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(cache.lookup(&key), None);
    }

    #[test]
    fn expired_h1_only_returns_none() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(0));
        let key = key("expired.example.com:80");
        cache.record_h1_only(key.clone());
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(cache.lookup(&key), None);
    }

    #[test]
    fn overwrite_h1_with_h2c() {
        let cache = H2cProbeCache::new();
        let key = key("host.com:80");
        cache.record_h1_only(key.clone());
        assert_eq!(cache.lookup(&key), Some(false));
        cache.record_h2c(key.clone());
        assert_eq!(cache.lookup(&key), Some(true));
    }

    #[test]
    fn multiple_authorities_independent() {
        let cache = H2cProbeCache::new();
        let a = key("a.com:80");
        let b = key("b.com:80");
        cache.record_h2c(a.clone());
        cache.record_h1_only(b.clone());
        assert_eq!(cache.lookup(&a), Some(true));
        assert_eq!(cache.lookup(&b), Some(false));
        assert_eq!(cache.lookup(&key("c.com:80")), None);
    }

    #[test]
    fn clone_shares_state() {
        let cache = H2cProbeCache::new();
        let cloned = cache.clone();
        let key = key("shared.com:80");
        cache.record_h2c(key.clone());
        assert_eq!(cloned.lookup(&key), Some(true));
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
        assert_eq!(cache.lookup(&key("example.com:80")), None);
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
        let key = key("poisoned.com:80");
        cache.record_h2c(key.clone());
        // Verify it didn't panic — reaching here means success
        assert_eq!(cache.lookup(&key), None);
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
        let key = key("poisoned.com:80");
        cache.record_h1_only(key.clone());
        // Verify it didn't panic — reaching here means success
        assert_eq!(cache.lookup(&key), None);
    }

    #[test]
    fn evicts_expired_when_over_capacity() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(1));
        {
            let mut map = cache.inner.lock().unwrap();
            for i in 0..66 {
                map.insert(
                    key(&format!("host{i}.com:80")),
                    H2cCapability::SupportsH2c {
                        probed_at: Instant::now() - Duration::from_millis(10),
                    },
                );
            }
        }

        // All entries are now expired. Next insert triggers eviction.
        cache.record_h2c(key("new.com:80"));
        let map = cache.inner.lock().unwrap();
        // Only the new entry should remain (all expired ones evicted)
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn route_and_forced_endpoint_are_part_of_probe_identity() {
        let cache = H2cProbeCache::new();
        let authority = authority("shared.example:80");
        let direct = H2cProbeKey::new(
            Scheme::HTTP,
            &authority,
            ProxyRoute::DIRECT,
            Some("127.0.0.1:8001".parse().unwrap()),
        );
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example:8080").unwrap();
        let proxied = H2cProbeKey::new(
            Scheme::HTTP,
            &authority,
            ProxyRoute::proxied(proxy.route_identity()),
            Some("127.0.0.1:8001".parse().unwrap()),
        );
        let other_endpoint = H2cProbeKey::new(
            Scheme::HTTP,
            &authority,
            ProxyRoute::DIRECT,
            Some("127.0.0.1:8002".parse().unwrap()),
        );

        cache.record_h2c(direct.clone());
        cache.record_h1_only(proxied.clone());

        assert_eq!(cache.lookup(&direct), Some(true));
        assert_eq!(cache.lookup(&proxied), Some(false));
        assert_eq!(cache.lookup(&other_endpoint), None);
    }
}
