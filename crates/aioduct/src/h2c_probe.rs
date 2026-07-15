use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use http::uri::{Authority, Scheme};

use crate::pool::ProxyRoute;

const DEFAULT_PROBE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
enum H2cCapability {
    SupportsH2c,
    H1Only,
}

struct H2cEndpointState {
    generation: u64,
    active_generation: u64,
    capability: Option<H2cCapability>,
    observed_at: Instant,
    active_probes: usize,
}

struct H2cProbeCacheState {
    endpoints: HashMap<H2cEndpointKey, H2cEndpointState>,
    next_generation: u64,
}

/// Identity of the transport path whose h2c capability was probed.
///
/// Capability belongs to the effective route and endpoint, not merely to the
/// origin authority: a direct address, a forced address, and a proxy route can
/// reach different HTTP implementations for the same URI.
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
        authority: Authority,
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

    pub(crate) fn endpoint(&self, remote_addr: SocketAddr) -> H2cEndpointKey {
        H2cEndpointKey {
            route: self.clone(),
            remote_addr,
            proxy_targets: None,
        }
    }

    pub(crate) fn proxy_endpoint(
        &self,
        remote_addr: SocketAddr,
        first_target_addr: Option<SocketAddr>,
        second_target_addr: Option<SocketAddr>,
    ) -> H2cEndpointKey {
        H2cEndpointKey {
            route: self.clone(),
            remote_addr,
            proxy_targets: Some(H2cProxyTargets {
                first_target_addr,
                second_target_addr,
            }),
        }
    }
}

/// One concrete network endpoint reached for an adaptive h2c route.
///
/// DNS and proxy endpoints behind one authority can have different protocol
/// capabilities during deployments. Keeping every locally known address in the
/// path prevents observations from one tunneled endpoint overwriting another.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) struct H2cEndpointKey {
    route: H2cProbeKey,
    remote_addr: SocketAddr,
    proxy_targets: Option<H2cProxyTargets>,
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
struct H2cProxyTargets {
    first_target_addr: Option<SocketAddr>,
    second_target_addr: Option<SocketAddr>,
}

/// Ownership token for one observation of a concrete adaptive-h2c endpoint.
///
/// Overlapping probes share a generation so current confirmed H2 evidence wins
/// over a concurrent H1 fallback regardless of start or completion order. An
/// expired observation or later non-overlapping generation can still observe a
/// genuine capability change.
pub(crate) struct H2cProbeToken {
    key: H2cEndpointKey,
    generation: u64,
    cache: Weak<Mutex<H2cProbeCacheState>>,
    active: bool,
}

impl Drop for H2cProbeToken {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let Some(cache) = self.cache.upgrade() else {
            return;
        };
        let Ok(mut state) = cache.lock() else {
            return;
        };
        H2cProbeCache::finish_probe(&mut state, &self.key);
    }
}

pub(crate) enum H2cProbeAction {
    UseH1,
    Probe(Box<H2cProbeToken>),
}

/// Caches per-route h2c capability probe results.
///
/// When using adaptive h2c, the first request to an unknown route endpoint
/// attempts an HTTP/2 prior-knowledge handshake. A successful probe caches h2c;
/// a confirmed HTTP/1 response caches h1-only. Subsequent requests to that same
/// route endpoint skip the probe.
#[derive(Clone)]
pub(crate) struct H2cProbeCache {
    inner: Arc<Mutex<H2cProbeCacheState>>,
    ttl: Duration,
}

impl H2cProbeCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(H2cProbeCacheState {
                endpoints: HashMap::new(),
                next_generation: 1,
            })),
            ttl: DEFAULT_PROBE_TTL,
        }
    }

    pub(crate) fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(H2cProbeCacheState {
                endpoints: HashMap::new(),
                next_generation: 1,
            })),
            ttl,
        }
    }

    /// Returns `Some(true)` if h2c is known, `Some(false)` if h1-only, `None` if unknown/expired.
    #[cfg(test)]
    pub(crate) fn lookup_endpoint(&self, key: &H2cEndpointKey) -> Option<bool> {
        let Ok(state) = self.inner.lock() else {
            return None;
        };
        let entry = state.endpoints.get(key)?;
        if entry.observed_at.elapsed() >= self.ttl {
            return None;
        }
        entry
            .capability
            .map(|capability| matches!(capability, H2cCapability::SupportsH2c))
    }

    pub(crate) fn begin_endpoint_probe(&self, key: H2cEndpointKey) -> H2cProbeAction {
        let Ok(mut state) = self.inner.lock() else {
            return H2cProbeAction::Probe(Box::new(H2cProbeToken {
                key,
                generation: 0,
                cache: Weak::new(),
                active: false,
            }));
        };
        if state.endpoints.get(&key).is_some_and(|entry| {
            entry.active_probes == 0
                && entry.observed_at.elapsed() < self.ttl
                && matches!(entry.capability, Some(H2cCapability::H1Only))
        }) {
            return H2cProbeAction::UseH1;
        }

        Self::evict_expired(&mut state, self.ttl);
        let active_generation = state
            .endpoints
            .get(&key)
            .and_then(|entry| (entry.active_probes > 0).then_some(entry.active_generation));
        let generation = active_generation.unwrap_or_else(|| {
            let generation = state.next_generation;
            state.next_generation = state.next_generation.wrapping_add(1).max(1);
            generation
        });
        let entry = state
            .endpoints
            .entry(key.clone())
            .or_insert_with(|| H2cEndpointState {
                generation: 0,
                active_generation: generation,
                capability: None,
                observed_at: Instant::now(),
                active_probes: 0,
            });
        if entry.active_probes == 0 {
            entry.active_generation = generation;
        }
        entry.active_probes += 1;
        drop(state);

        H2cProbeAction::Probe(Box::new(H2cProbeToken {
            key,
            generation,
            cache: Arc::downgrade(&self.inner),
            active: true,
        }))
    }

    pub(crate) fn confirm_h2c_endpoint(&self, token: H2cProbeToken) {
        self.complete_probe(token, Some(H2cCapability::SupportsH2c));
    }

    pub(crate) fn reject_h2c_endpoint(&self, token: &H2cProbeToken) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        Self::publish_observation(&mut state, token, None, self.ttl);
    }

    pub(crate) fn confirm_h1_endpoint(&self, token: H2cProbeToken) {
        self.complete_probe(token, Some(H2cCapability::H1Only));
    }

    fn complete_probe(&self, mut token: H2cProbeToken, capability: Option<H2cCapability>) {
        if let Ok(mut state) = self.inner.lock() {
            Self::publish_observation(&mut state, &token, capability, self.ttl);
            Self::finish_probe(&mut state, &token.key);
        }
        token.active = false;
    }

    fn publish_observation(
        state: &mut H2cProbeCacheState,
        token: &H2cProbeToken,
        capability: Option<H2cCapability>,
        ttl: Duration,
    ) {
        let Some(entry) = state.endpoints.get_mut(&token.key) else {
            return;
        };
        if token.generation < entry.generation {
            return;
        }
        if token.generation == entry.generation
            && matches!(entry.capability, Some(H2cCapability::SupportsH2c))
            && entry.observed_at.elapsed() < ttl
            && !matches!(capability, Some(H2cCapability::SupportsH2c))
        {
            return;
        }
        entry.generation = token.generation;
        entry.capability = capability;
        entry.observed_at = Instant::now();
    }

    fn finish_probe(state: &mut H2cProbeCacheState, key: &H2cEndpointKey) {
        let Some(entry) = state.endpoints.get_mut(key) else {
            return;
        };
        debug_assert!(entry.active_probes > 0);
        entry.active_probes = entry.active_probes.saturating_sub(1);
    }

    fn evict_expired(state: &mut H2cProbeCacheState, ttl: Duration) {
        if state.endpoints.len() <= 64 {
            return;
        }
        state
            .endpoints
            .retain(|_, entry| entry.active_probes > 0 || entry.observed_at.elapsed() < ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(s: &str) -> Authority {
        s.parse().unwrap()
    }

    fn direct_key(s: &str) -> H2cProbeKey {
        H2cProbeKey::new(Scheme::HTTP, authority(s), ProxyRoute::DIRECT, None)
    }

    fn direct_endpoint(s: &str, remote_addr: &str) -> H2cEndpointKey {
        direct_key(s).endpoint(remote_addr.parse().unwrap())
    }

    fn begin_probe(cache: &H2cProbeCache, key: H2cEndpointKey) -> H2cProbeToken {
        match cache.begin_endpoint_probe(key) {
            H2cProbeAction::Probe(token) => *token,
            H2cProbeAction::UseH1 => panic!("expected endpoint to be probeable"),
        }
    }

    fn record_h2c(cache: &H2cProbeCache, key: H2cEndpointKey) {
        let token = begin_probe(cache, key);
        cache.confirm_h2c_endpoint(token);
    }

    fn record_h1(cache: &H2cProbeCache, key: H2cEndpointKey) {
        let token = begin_probe(cache, key);
        cache.reject_h2c_endpoint(&token);
        cache.confirm_h1_endpoint(token);
    }

    #[test]
    fn unknown_returns_none() {
        let cache = H2cProbeCache::new();
        assert_eq!(
            cache.lookup_endpoint(&direct_endpoint("example.com:80", "127.0.0.1:80")),
            None
        );
    }

    #[test]
    fn record_h2c_returns_true() {
        let cache = H2cProbeCache::new();
        let key = direct_endpoint("grpc.example.com:50051", "127.0.0.1:50051");
        record_h2c(&cache, key.clone());
        assert_eq!(cache.lookup_endpoint(&key), Some(true));
    }

    #[test]
    fn record_h1_only_returns_false() {
        let cache = H2cProbeCache::new();
        let key = direct_endpoint("legacy.example.com:80", "127.0.0.1:80");
        record_h1(&cache, key.clone());
        assert_eq!(cache.lookup_endpoint(&key), Some(false));
    }

    #[test]
    fn expired_entry_returns_none() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(0));
        let key = direct_endpoint("expired.example.com:80", "127.0.0.1:80");
        record_h2c(&cache, key.clone());
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(cache.lookup_endpoint(&key), None);
    }

    #[test]
    fn expired_h1_only_returns_none() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(0));
        let key = direct_endpoint("expired.example.com:80", "127.0.0.1:80");
        record_h1(&cache, key.clone());
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(cache.lookup_endpoint(&key), None);
    }

    #[test]
    fn expired_h1_can_be_reprobed_as_h2c() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(50));
        let key = direct_endpoint("host.com:80", "127.0.0.1:80");
        record_h1(&cache, key.clone());
        assert_eq!(cache.lookup_endpoint(&key), Some(false));
        std::thread::sleep(Duration::from_millis(60));
        record_h2c(&cache, key.clone());
        assert_eq!(cache.lookup_endpoint(&key), Some(true));
    }

    #[test]
    fn multiple_authorities_independent() {
        let cache = H2cProbeCache::new();
        let a = direct_endpoint("a.com:80", "127.0.0.1:80");
        let b = direct_endpoint("b.com:80", "127.0.0.2:80");
        let c = direct_endpoint("c.com:80", "127.0.0.3:80");
        record_h2c(&cache, a.clone());
        record_h1(&cache, b.clone());
        assert_eq!(cache.lookup_endpoint(&a), Some(true));
        assert_eq!(cache.lookup_endpoint(&b), Some(false));
        assert_eq!(cache.lookup_endpoint(&c), None);
    }

    #[test]
    fn clone_shares_state() {
        let cache = H2cProbeCache::new();
        let cloned = cache.clone();
        let key = direct_endpoint("shared.com:80", "127.0.0.1:80");
        record_h2c(&cache, key.clone());
        assert_eq!(cloned.lookup_endpoint(&key), Some(true));
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
        assert_eq!(
            cache.lookup_endpoint(&direct_endpoint("example.com:80", "127.0.0.1:80")),
            None
        );
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
        let key = direct_endpoint("poisoned.com:80", "127.0.0.1:80");
        let token = begin_probe(&cache, key.clone());
        cache.confirm_h2c_endpoint(token);
        // Verify it didn't panic — reaching here means success
        assert_eq!(cache.lookup_endpoint(&key), None);
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
        let key = direct_endpoint("poisoned.com:80", "127.0.0.1:80");
        let token = begin_probe(&cache, key.clone());
        cache.reject_h2c_endpoint(&token);
        cache.confirm_h1_endpoint(token);
        // Verify it didn't panic — reaching here means success
        assert_eq!(cache.lookup_endpoint(&key), None);
    }

    #[test]
    fn evicts_expired_when_over_capacity() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(1));
        {
            let mut state = cache.inner.lock().unwrap();
            for i in 0..66 {
                state.endpoints.insert(
                    direct_endpoint(&format!("host{i}.com:80"), "127.0.0.1:80"),
                    H2cEndpointState {
                        generation: i + 1,
                        active_generation: 0,
                        capability: Some(H2cCapability::SupportsH2c),
                        observed_at: Instant::now() - Duration::from_millis(10),
                        active_probes: 0,
                    },
                );
            }
            state.next_generation = 67;
        }

        // All entries are now expired. Next insert triggers eviction.
        record_h2c(&cache, direct_endpoint("new.com:80", "127.0.0.1:80"));
        let state = cache.inner.lock().unwrap();
        // Only the new entry should remain (all expired ones evicted)
        assert_eq!(state.endpoints.len(), 1);
    }

    #[test]
    fn route_identity_segregates_capabilities() {
        let cache = H2cProbeCache::new();
        let authority = authority("shared.example:80");
        let direct = H2cProbeKey::new(Scheme::HTTP, authority.clone(), ProxyRoute::DIRECT, None);
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example:8080").unwrap();
        let proxied = H2cProbeKey::new(
            Scheme::HTTP,
            authority,
            ProxyRoute::proxied(proxy.route_identity()),
            None,
        );

        let remote_addr = "127.0.0.1:8080".parse().unwrap();
        let direct = direct.endpoint(remote_addr);
        let proxied = proxied.endpoint(remote_addr);
        record_h2c(&cache, direct.clone());
        record_h1(&cache, proxied.clone());

        assert_eq!(cache.lookup_endpoint(&direct), Some(true));
        assert_eq!(cache.lookup_endpoint(&proxied), Some(false));
    }

    #[test]
    fn proxy_target_path_segregates_capabilities() {
        let cache = H2cProbeCache::new();
        let proxy = crate::proxy::ProxyConfig::socks5("socks5://127.0.0.1:1080").unwrap();
        let route = H2cProbeKey::new(
            Scheme::HTTP,
            authority("shared.example:80"),
            ProxyRoute::proxied(proxy.route_identity()),
            None,
        );
        let first_proxy = "127.0.0.1:1080".parse().unwrap();
        let first_path = route.proxy_endpoint(
            first_proxy,
            Some("192.0.2.10:8080".parse().unwrap()),
            Some("198.51.100.10:80".parse().unwrap()),
        );
        let different_first_target = route.proxy_endpoint(
            first_proxy,
            Some("192.0.2.11:8080".parse().unwrap()),
            Some("198.51.100.10:80".parse().unwrap()),
        );
        let different_second_target = route.proxy_endpoint(
            first_proxy,
            Some("192.0.2.10:8080".parse().unwrap()),
            Some("198.51.100.11:80".parse().unwrap()),
        );

        record_h1(&cache, first_path.clone());
        record_h2c(&cache, different_first_target.clone());
        record_h2c(&cache, different_second_target.clone());

        assert_eq!(cache.lookup_endpoint(&first_path), Some(false));
        assert_eq!(cache.lookup_endpoint(&different_first_target), Some(true));
        assert_eq!(cache.lookup_endpoint(&different_second_target), Some(true));
    }

    #[test]
    fn forced_endpoint_segregates_capabilities() {
        let cache = H2cProbeCache::new();
        let authority = authority("shared.example:80");
        let h2 = H2cProbeKey::new(
            Scheme::HTTP,
            authority.clone(),
            ProxyRoute::DIRECT,
            Some("127.0.0.1:8001".parse().unwrap()),
        );
        let h1 = H2cProbeKey::new(
            Scheme::HTTP,
            authority,
            ProxyRoute::DIRECT,
            Some("127.0.0.1:8002".parse().unwrap()),
        );

        let h2 = h2.endpoint("127.0.0.1:8001".parse().unwrap());
        let h1 = h1.endpoint("127.0.0.1:8002".parse().unwrap());
        record_h2c(&cache, h2.clone());
        record_h1(&cache, h1.clone());

        assert_eq!(cache.lookup_endpoint(&h2), Some(true));
        assert_eq!(cache.lookup_endpoint(&h1), Some(false));
    }

    #[test]
    fn scheme_and_default_port_are_part_of_probe_identity() {
        let cache = H2cProbeCache::new();
        let implicit_http = H2cProbeKey::new(
            Scheme::HTTP,
            authority("shared.example"),
            ProxyRoute::DIRECT,
            None,
        );
        let explicit_http = H2cProbeKey::new(
            Scheme::HTTP,
            authority("shared.example:80"),
            ProxyRoute::DIRECT,
            None,
        );
        let https = H2cProbeKey::new(
            Scheme::HTTPS,
            authority("shared.example:80"),
            ProxyRoute::DIRECT,
            None,
        );

        let remote_addr = "127.0.0.1:80".parse().unwrap();
        record_h2c(&cache, implicit_http.endpoint(remote_addr));
        assert_eq!(
            cache.lookup_endpoint(&explicit_http.endpoint(remote_addr)),
            Some(true)
        );
        assert_eq!(cache.lookup_endpoint(&https.endpoint(remote_addr)), None);
    }

    #[test]
    fn selected_endpoints_do_not_overwrite_each_other() {
        let cache = H2cProbeCache::new();
        let route = direct_key("mixed.example:80");
        let h2 = route.endpoint("192.0.2.10:80".parse().unwrap());
        let h1 = route.endpoint("192.0.2.11:80".parse().unwrap());

        let first = {
            let cache = cache.clone();
            let h2 = h2.clone();
            std::thread::spawn(move || record_h2c(&cache, h2))
        };
        let second = {
            let cache = cache.clone();
            let h1 = h1.clone();
            std::thread::spawn(move || record_h1(&cache, h1))
        };
        first.join().unwrap();
        second.join().unwrap();

        assert_eq!(cache.lookup_endpoint(&h2), Some(true));
        assert_eq!(cache.lookup_endpoint(&h1), Some(false));
    }

    #[test]
    fn invalidating_one_endpoint_preserves_the_other() {
        let cache = H2cProbeCache::new();
        let route = direct_key("mixed.example:80");
        let h2 = route.endpoint("192.0.2.10:80".parse().unwrap());
        let h1 = route.endpoint("192.0.2.11:80".parse().unwrap());
        record_h2c(&cache, h2.clone());
        record_h1(&cache, h1.clone());

        let token = begin_probe(&cache, h2.clone());
        cache.reject_h2c_endpoint(&token);
        drop(token);

        assert_eq!(cache.lookup_endpoint(&h2), None);
        assert_eq!(cache.lookup_endpoint(&h1), Some(false));
    }

    #[test]
    fn stale_h1_fallback_cannot_replace_newer_h2_confirmation() {
        let cache = H2cProbeCache::new();
        let key = direct_endpoint("racing.example:80", "127.0.0.1:80");

        let stale = begin_probe(&cache, key.clone());
        cache.reject_h2c_endpoint(&stale);
        let newer = begin_probe(&cache, key.clone());
        cache.confirm_h2c_endpoint(newer);
        cache.confirm_h1_endpoint(stale);

        assert_eq!(cache.lookup_endpoint(&key), Some(true));
    }

    #[test]
    fn newer_overlapping_h1_fallback_cannot_replace_older_h2_confirmation() {
        let cache = H2cProbeCache::new();
        let key = direct_endpoint("reverse-racing.example:80", "127.0.0.1:80");

        let h2 = begin_probe(&cache, key.clone());
        let h1 = begin_probe(&cache, key.clone());
        cache.reject_h2c_endpoint(&h1);
        cache.confirm_h2c_endpoint(h2);
        cache.confirm_h1_endpoint(h1);

        assert_eq!(cache.lookup_endpoint(&key), Some(true));
    }

    #[test]
    fn overlapping_h1_can_replace_expired_h2_confirmation() {
        let cache = H2cProbeCache::with_ttl(Duration::from_millis(50));
        let key = direct_endpoint("expired-race.example:80", "127.0.0.1:80");

        let h2 = begin_probe(&cache, key.clone());
        let h1 = begin_probe(&cache, key.clone());
        cache.confirm_h2c_endpoint(h2);
        std::thread::sleep(Duration::from_millis(60));
        cache.reject_h2c_endpoint(&h1);
        cache.confirm_h1_endpoint(h1);

        assert_eq!(cache.lookup_endpoint(&key), Some(false));
    }

    #[test]
    fn newer_h1_observation_can_replace_older_h2_confirmation() {
        let cache = H2cProbeCache::new();
        let key = direct_endpoint("changed.example:80", "127.0.0.1:80");
        record_h2c(&cache, key.clone());

        let newer = begin_probe(&cache, key.clone());
        cache.reject_h2c_endpoint(&newer);
        cache.confirm_h1_endpoint(newer);

        assert_eq!(cache.lookup_endpoint(&key), Some(false));
    }
}
