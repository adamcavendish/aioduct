/// Connection pool module with types for managing idle connections.
pub(crate) mod connection;

pub(crate) use connection::{HttpConnection, PooledConnection};

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use http::uri::{Authority, Scheme};

use crate::runtime::RuntimePoll;

const DEFAULT_MAX_IDLE_PER_HOST: usize = 10;
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Protocol version hint for pool key segregation.
#[derive(Clone, Copy, Debug, Default, Hash, Eq, PartialEq)]
pub(crate) enum ProtocolHint {
    /// No preference — use whatever the connection negotiates.
    #[default]
    Auto,
    /// Force HTTP/2 prior knowledge (h2c).
    H2c,
    /// Adaptive: try h2c, fall back to h1 if rejected. Caches the result.
    AdaptiveH2c,
}

/// Stable identity for a proxy route, used to segregate pooled connections
/// that reach the same origin through different proxy configurations.
///
/// 0 means direct (no proxy). Non-zero is a hash of the proxy chain or
/// per-request proxy config.
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Default)]
pub(crate) struct ProxyRoute(u64);

impl ProxyRoute {
    /// Sentinel value for direct (non-proxied) connections.
    pub(crate) const DIRECT: Self = Self(0);

    /// Build a route identity from a pre-computed hash.
    pub(crate) fn from_hash(hash: u64) -> Self {
        Self(hash)
    }
}

/// Connection pool key identifying a (scheme, authority, protocol, proxy-route)
/// quadruple.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) struct PoolKey {
    /// The URI scheme (http or https).
    pub(crate) scheme: Scheme,
    /// The URI authority (host and optional port).
    pub(crate) authority: Authority,
    /// Protocol hint for pool segregation.
    pub(crate) protocol: ProtocolHint,
    /// Proxy route identity: DIRECT for no proxy, hashed from the effective
    /// proxy configuration otherwise.
    pub(crate) proxy_route: ProxyRoute,
}

impl PoolKey {
    /// Create a new pool key with the default protocol hint (Auto) and direct route.
    #[allow(dead_code)]
    pub(crate) fn new(scheme: Scheme, authority: Authority) -> Self {
        Self {
            scheme,
            authority,
            protocol: ProtocolHint::Auto,
            proxy_route: ProxyRoute::DIRECT,
        }
    }

    /// Create a pool key with a protocol hint and direct route.
    #[allow(dead_code)]
    pub(crate) fn with_hint(scheme: Scheme, authority: Authority, protocol: ProtocolHint) -> Self {
        Self {
            scheme,
            authority,
            protocol,
            proxy_route: ProxyRoute::DIRECT,
        }
    }

    /// Create a pool key with both a protocol hint and a proxy route identity.
    pub(crate) fn with_hint_and_route(
        scheme: Scheme,
        authority: Authority,
        protocol: ProtocolHint,
        proxy_route: ProxyRoute,
    ) -> Self {
        Self {
            scheme,
            authority,
            protocol,
            proxy_route,
        }
    }
}

/// Pool diagnostic counters. All counters are monotonic since engine creation.
/// Uses atomics so the hot checkout/checkin path avoids extra mutex contention.
struct PoolCounters {
    checkout_hits: AtomicU64,
    checkout_coalesced_hits: AtomicU64,
    checkout_misses: AtomicU64,
    stale_reuse_retries: AtomicU64,
    idle_timeout_evictions: AtomicU64,
    max_lifetime_evictions: AtomicU64,
    checkout_not_ready_evictions: AtomicU64,
    capacity_evictions: AtomicU64,
}

impl PoolCounters {
    fn new() -> Self {
        Self {
            checkout_hits: AtomicU64::new(0),
            checkout_coalesced_hits: AtomicU64::new(0),
            checkout_misses: AtomicU64::new(0),
            stale_reuse_retries: AtomicU64::new(0),
            idle_timeout_evictions: AtomicU64::new(0),
            max_lifetime_evictions: AtomicU64::new(0),
            checkout_not_ready_evictions: AtomicU64::new(0),
            capacity_evictions: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> PoolStatsCounters {
        PoolStatsCounters {
            checkout_hits: self.checkout_hits.load(Ordering::Relaxed),
            checkout_coalesced_hits: self.checkout_coalesced_hits.load(Ordering::Relaxed),
            checkout_misses: self.checkout_misses.load(Ordering::Relaxed),
            stale_reuse_retries: self.stale_reuse_retries.load(Ordering::Relaxed),
            idle_timeout_evictions: self.idle_timeout_evictions.load(Ordering::Relaxed),
            max_lifetime_evictions: self.max_lifetime_evictions.load(Ordering::Relaxed),
            checkout_not_ready_evictions: self.checkout_not_ready_evictions.load(Ordering::Relaxed),
            capacity_evictions: self.capacity_evictions.load(Ordering::Relaxed),
        }
    }
}

/// Non-atomic snapshot of pool counters (loaded under mutex during snapshot).
struct PoolStatsCounters {
    checkout_hits: u64,
    checkout_coalesced_hits: u64,
    checkout_misses: u64,
    stale_reuse_retries: u64,
    idle_timeout_evictions: u64,
    max_lifetime_evictions: u64,
    checkout_not_ready_evictions: u64,
    capacity_evictions: u64,
}

/// Snapshot of pool connection statistics. All counters are monotonic since
/// engine creation. Counts reflect pool-internal handle tracking, which may
/// differ from physical connection counts for H2/H3 multiplexed transports.
#[derive(Clone, Debug)]
pub struct PoolStats {
    /// Checkouts that found an idle connection in the pool.
    pub checkout_hits: u64,
    /// Checkouts that reused an H2/H3 connection via SAN-based coalescing
    /// (RFC 7540 §9.1.1). Always 0 on Local engines.
    pub checkout_coalesced_hits: u64,
    /// Requests that exhausted all pool paths and required a fresh connection.
    pub checkout_misses: u64,
    /// Connections detected as stale mid-request and transparently retried.
    pub stale_reuse_retries: u64,
    /// Connections evicted due to idle timeout expiry.
    pub idle_timeout_evictions: u64,
    /// Connections evicted due to exceeding their maximum lifetime.
    pub max_lifetime_evictions: u64,
    /// Connections discarded at checkout because `is_ready()` returned false.
    pub checkout_not_ready_evictions: u64,
    /// Connections evicted because the per-host idle queue was at capacity.
    pub capacity_evictions: u64,
    /// Number of idle pool handles across all hosts.
    pub idle_pool_entries: usize,
    /// Number of checked-out pool handles across all hosts.
    pub checked_out_pool_handles: usize,
    /// Per-host breakdown, sorted by (scheme, authority).
    pub hosts: Vec<PoolHostStats>,
}

/// Per-host pool inventory snapshot.
#[derive(Clone, Debug)]
pub struct PoolHostStats {
    /// URI scheme (http or https).
    pub scheme: String,
    /// URI authority (host and optional port).
    pub authority: String,
    /// Protocol hint used as pool key discriminator (Auto/H2c/AdaptiveH2c).
    pub protocol_hint: String,
    /// Proxy route identifier: "direct" if no proxy, otherwise an opaque label.
    pub route: String,
    /// Idle pool handles for this host.
    pub idle: usize,
    /// Checked-out pool handles for this host.
    pub active: usize,
}

struct IdleConnection<B> {
    connection: PooledConnection<B>,
    idle_since: Instant,
}

pub(crate) struct PoolInner<B> {
    idle: HashMap<PoolKey, VecDeque<IdleConnection<B>>>,
    /// Reverse index: SAN → set of pool keys whose connections cover that name.
    san_index: HashMap<String, HashSet<PoolKey>>,
    /// Pool keys with an in-progress H2/H3 connection attempt.
    connecting_h2: HashSet<PoolKey>,
    max_idle_per_host: usize,
    max_active_per_host: Option<NonZeroUsize>,
    max_active_streams_per_connection: Option<NonZeroUsize>,
    idle_timeout: Duration,
    max_lifetime: Option<Duration>,
    /// Count of active (checked-out, not yet returned) connections per host key.
    active: HashMap<PoolKey, usize>,
}

/// Reservation for a fresh connection attempt counted against the per-host
/// active cap. Dropping it releases the slot unless ownership was transferred
/// to a [`PooledConnection`].
pub(crate) struct ActiveReservation<B> {
    inner: Weak<Mutex<PoolInner<B>>>,
    key: Option<PoolKey>,
}

impl<B> ActiveReservation<B> {
    fn new(inner: Weak<Mutex<PoolInner<B>>>, key: PoolKey) -> Self {
        Self {
            inner,
            key: Some(key),
        }
    }

    fn disarm(&mut self) -> Option<PoolKey> {
        self.key.take()
    }
}

impl<B> Drop for ActiveReservation<B> {
    fn drop(&mut self) {
        if let Some(ref key) = self.key
            && let Some(pool_inner) = self.inner.upgrade()
            && let Ok(mut inner) = pool_inner.lock()
        {
            decrement_active(&mut inner, key);
        }
    }
}

pub(crate) fn decrement_active<B>(inner: &mut PoolInner<B>, key: &PoolKey) {
    if let Some(count) = inner.active.get_mut(key) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            inner.active.remove(key);
        }
    }
}

/// Thread-safe pool of idle HTTP connections keyed by origin.
pub(crate) struct ConnectionPool<B> {
    inner: Arc<Mutex<PoolInner<B>>>,
    reaper_spawned: Arc<AtomicBool>,
    /// Pool-level diagnostic counters. Atomically updated outside the mutex
    /// so the hot checkout/checkin path avoids extra contention.
    counters: Arc<PoolCounters>,
}

impl<B> Clone for ConnectionPool<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            reaper_spawned: Arc::clone(&self.reaper_spawned),
            counters: Arc::clone(&self.counters),
        }
    }
}

impl<B: 'static> ConnectionPool<B> {
    /// Create a pool with default settings.
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                idle: HashMap::new(),
                san_index: HashMap::new(),
                connecting_h2: HashSet::new(),
                max_idle_per_host: DEFAULT_MAX_IDLE_PER_HOST,
                max_active_per_host: None,
                max_active_streams_per_connection: None,
                idle_timeout: DEFAULT_IDLE_TIMEOUT,
                max_lifetime: None,
                active: HashMap::new(),
            })),
            reaper_spawned: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(PoolCounters::new()),
        }
    }

    /// Set the maximum idle connections per host.
    pub(crate) fn with_max_idle_per_host(self, max_idle_per_host: usize) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.max_idle_per_host = max_idle_per_host;
        }
        self
    }

    /// Set the idle timeout for pooled connections.
    pub(crate) fn with_idle_timeout(self, idle_timeout: Duration) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.idle_timeout = idle_timeout;
        }
        self
    }

    /// Set the maximum lifetime for pooled connections.
    pub(crate) fn with_max_lifetime(self, max_lifetime: Duration) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.max_lifetime = Some(max_lifetime);
        }
        self
    }

    /// Set the maximum active connections per host. `None` means unlimited.
    pub(crate) fn with_max_active_per_host(self, max: Option<NonZeroUsize>) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.max_active_per_host = max;
        }
        self
    }

    /// Returns whether an active slot is currently available for tests.
    ///
    /// Fresh connection attempts must use `try_reserve_active` so checking and
    /// incrementing the active count happen atomically.
    #[cfg(test)]
    pub(crate) fn can_connect(&self, key: &PoolKey) -> bool {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        let max = match inner.max_active_per_host {
            Some(max) => max.get(),
            None => return true,
        };
        inner.active.get(key).copied().unwrap_or(0) < max
    }

    /// Reserve an active slot for a new connection attempt.
    ///
    /// Unlike `can_connect`, this is atomic with respect to the active counter,
    /// so concurrent fresh dials cannot all pass the cap check before any of
    /// them has been checked into or out of the pool.
    ///
    /// Returns a [`PoolLimitError`] when the configured `max_active_per_host`
    /// cap is already reached for `key`.
    pub(crate) fn try_reserve_active(
        &self,
        key: &PoolKey,
    ) -> Result<ActiveReservation<B>, crate::error::PoolLimitError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(max) = inner.max_active_per_host {
            let active = inner.active.get(key).copied().unwrap_or(0);
            if active >= max.get() {
                return Err(crate::error::PoolLimitError::new(
                    crate::error::PoolLimitKind::MaxActivePerHost,
                    Some(max.get()),
                ));
            }
        }

        *inner.active.entry(key.clone()).or_insert(0) += 1;
        Ok(ActiveReservation::new(
            Arc::downgrade(&self.inner),
            key.clone(),
        ))
    }

    /// Transfer a fresh-connection reservation onto the pooled connection so
    /// check-in or drop releases the active slot exactly once.
    pub(crate) fn attach_active_reservation(
        &self,
        connection: &mut PooledConnection<B>,
        reservation: &mut ActiveReservation<B>,
    ) {
        if let Some(key) = reservation.disarm() {
            connection.pool = Arc::downgrade(&self.inner);
            connection.key = Some(key);
        }
    }

    /// Move a connection's active count from one pool key to another.
    ///
    /// Used when an adaptive h2c probe falls back to H1: the reservation was
    /// made under the `H2c` key but the connection ends up on the `Auto` key.
    /// Without this migration the `H2c` entry leaks and the `Auto` key sees
    /// an undercount.
    pub(crate) fn rekey_active(&self, old_key: &PoolKey, new_key: &PoolKey) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        decrement_active(&mut inner, old_key);
        *inner.active.entry(new_key.clone()).or_insert(0) += 1;
    }

    /// Set the maximum active H2/H3 streams allowed per pooled connection.
    pub(crate) fn with_max_active_streams_per_connection(self, max_active: NonZeroUsize) -> Self {
        if let Ok(mut inner) = self.inner.lock() {
            inner.max_active_streams_per_connection = Some(max_active);
        }
        self
    }

    /// Returns the configured active H2/H3 stream limit per pooled connection.
    pub(crate) fn max_active_streams_per_connection(&self) -> Option<NonZeroUsize> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .max_active_streams_per_connection
    }

    /// Disable spawning the background reaper task.
    ///
    /// This is useful for unit tests that don't need the reaper and may not
    /// have a full async runtime available.
    #[cfg(any(test, feature = "__bench"))]
    pub(crate) fn without_reaper(self) -> Self {
        self.reaper_spawned.store(true, Ordering::Relaxed);
        self
    }

    /// Returns the configured idle timeout for this pool.
    pub(crate) fn idle_timeout(&self) -> Duration {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .idle_timeout
    }

    /// Returns the configured maximum connection lifetime for this pool.
    #[cfg(all(test, feature = "tokio"))]
    pub(crate) fn max_lifetime(&self) -> Option<Duration> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .max_lifetime
    }

    fn connection_within_lifetime(
        connection: &PooledConnection<B>,
        max_lifetime: Option<Duration>,
    ) -> bool {
        match max_lifetime {
            Some(max_lifetime) => connection.created_at.elapsed() < max_lifetime,
            None => true,
        }
    }

    /// Retrieve an idle, ready connection for the given key.
    ///
    /// Uses LIFO ordering (most recently returned first) and checks readiness
    /// on each candidate, trying all pooled connections before giving up.
    pub(crate) fn checkout(&self, key: &PoolKey) -> Option<PooledConnection<B>> {
        self.checkout_matching(key, |_| true)
    }

    /// Retrieve only a pooled HTTP/3 connection while preserving other
    /// transports stored under the origin's automatic protocol key.
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) fn checkout_h3(&self, key: &PoolKey) -> Option<PooledConnection<B>> {
        self.checkout_matching(key, |connection| connection.is_h3())
    }

    fn checkout_matching(
        &self,
        key: &PoolKey,
        matches: impl Fn(&PooledConnection<B>) -> bool,
    ) -> Option<PooledConnection<B>> {
        let pool_weak = Arc::downgrade(&self.inner);
        let mut inner = self.inner.lock().ok()?;

        // Gate checkout on max_active_per_host before touching the idle queue.
        // If the cap is reached, return None so the caller falls through to a
        // fresh dial — which try_reserve_active gates with the typed error.
        if let Some(max) = inner.max_active_per_host
            && inner.active.get(key).copied().unwrap_or(0) >= max.get()
        {
            return None;
        }

        let idle_timeout = inner.idle_timeout;
        let max_lifetime = inner.max_lifetime;
        let max_active = inner.max_active_streams_per_connection;

        // Scope queue borrow so inner can be accessed afterwards.
        let (result, remove_idle_key) = {
            let queue = inner.idle.get_mut(key)?;
            let now = Instant::now();
            let mut retained_unavailable = Vec::new();
            let mut result = None;
            let mut remove_idle_key = false;

            while let Some(entry) = queue.pop_back() {
                if now.duration_since(entry.idle_since) >= idle_timeout {
                    self.counters
                        .idle_timeout_evictions
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if !Self::connection_within_lifetime(&entry.connection, max_lifetime) {
                    self.counters
                        .max_lifetime_evictions
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if !matches(&entry.connection) {
                    retained_unavailable.push(entry);
                    continue;
                }
                if entry.connection.is_ready() {
                    if entry.connection.is_h2_or_h3() {
                        let mut entry = entry;
                        if let Some(mut cloned) =
                            entry.connection.clone_for_multiplex_with_limit(max_active)
                        {
                            cloned.pool = pool_weak.clone();
                            cloned.key = Some(key.clone());
                            entry.connection.pool = Weak::new();
                            entry.connection.key = None;
                            entry.idle_since = now;
                            // drain avoids double-move; after drain the vec is empty
                            queue.extend(retained_unavailable.drain(..).rev());
                            queue.push_back(entry);
                            result = Some(cloned);
                            break;
                        }
                        entry.idle_since = now;
                        retained_unavailable.push(entry);
                        continue;
                    }
                    queue.extend(retained_unavailable.drain(..).rev());
                    remove_idle_key = queue.is_empty();
                    let mut conn = entry.connection;
                    conn.pool = pool_weak.clone();
                    conn.key = Some(key.clone());
                    result = Some(conn);
                    break;
                }
                self.counters
                    .checkout_not_ready_evictions
                    .fetch_add(1, Ordering::Relaxed);
            }

            if result.is_none() {
                queue.extend(retained_unavailable.drain(..).rev());
                remove_idle_key = queue.is_empty();
            }

            (result, remove_idle_key)
        };

        // Queue borrow released — safe to mutate inner again.
        if remove_idle_key {
            inner.idle.remove(key);
        }
        if result.is_some() {
            *inner.active.entry(key.clone()).or_insert(0) += 1;
        }
        result
    }

    /// Return a connection to the pool for future reuse.
    ///
    /// When at capacity, evicts the oldest idle connection to make room.
    pub(crate) fn checkin(&self, key: PoolKey, mut connection: PooledConnection<B>) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };

        // Decrement the active count tracked for this connection.
        let active_key = connection.key.clone();
        if let Some(ref k) = active_key {
            decrement_active(&mut inner, k);
        }

        // Clear pool/key so Drop does not double-decrement.
        connection.pool = Weak::new();
        connection.key = None;

        let max = inner.max_idle_per_host;

        if max == 0 {
            return;
        }

        if !Self::connection_within_lifetime(&connection, inner.max_lifetime) {
            self.counters
                .max_lifetime_evictions
                .fetch_add(1, Ordering::Relaxed);
            return;
        }

        for san in connection.sans.iter() {
            inner
                .san_index
                .entry(san.clone())
                .or_default()
                .insert(key.clone());
        }

        let queue = inner.idle.entry(key).or_default();

        if queue.len() >= max {
            queue.pop_front();
            self.counters
                .capacity_evictions
                .fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(IdleConnection {
            connection,
            idle_since: Instant::now(),
        });
    }

    /// Record a checkout hit (pool reuse) at the request level.
    pub(crate) fn record_checkout_hit(&self) {
        self.counters.checkout_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a coalesced checkout hit at the request level.
    pub(crate) fn record_checkout_coalesced_hit(&self) {
        self.counters
            .checkout_coalesced_hits
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a checkout miss (fresh connection required) at the request level.
    pub(crate) fn record_checkout_miss(&self) {
        self.counters
            .checkout_misses
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a stale reuse retry at the request level.
    pub(crate) fn record_stale_reuse_retry(&self) {
        self.counters
            .stale_reuse_retries
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Take a snapshot of pool statistics.
    pub(crate) fn snapshot(&self) -> PoolStats {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let counters = self.counters.snapshot();
        let idle_pool_entries: usize = inner.idle.values().map(|q| q.len()).sum();
        let checked_out_pool_handles: usize = inner.active.values().sum();

        let mut hosts: Vec<PoolHostStats> = inner
            .idle
            .iter()
            .map(|(key, queue)| {
                let active = inner.active.get(key).copied().unwrap_or(0);
                let route = if key.proxy_route.0 == 0 {
                    "direct".to_owned()
                } else {
                    format!("{:x}", key.proxy_route.0)
                };
                PoolHostStats {
                    scheme: key.scheme.to_string(),
                    authority: key.authority.to_string(),
                    protocol_hint: format!("{:?}", key.protocol),
                    route,
                    idle: queue.len(),
                    active,
                }
            })
            .collect();

        // Include hosts that only have active connections (no idle queue).
        for (key, &active) in &inner.active {
            if !inner.idle.contains_key(key) && active > 0 {
                let route = if key.proxy_route.0 == 0 {
                    "direct".to_owned()
                } else {
                    format!("{:x}", key.proxy_route.0)
                };
                hosts.push(PoolHostStats {
                    scheme: key.scheme.to_string(),
                    authority: key.authority.to_string(),
                    protocol_hint: format!("{:?}", key.protocol),
                    route,
                    idle: 0,
                    active,
                });
            }
        }

        hosts.sort_by(|a, b| {
            a.scheme
                .cmp(&b.scheme)
                .then_with(|| a.authority.cmp(&b.authority))
        });

        PoolStats {
            checkout_hits: counters.checkout_hits,
            checkout_coalesced_hits: counters.checkout_coalesced_hits,
            checkout_misses: counters.checkout_misses,
            stale_reuse_retries: counters.stale_reuse_retries,
            idle_timeout_evictions: counters.idle_timeout_evictions,
            max_lifetime_evictions: counters.max_lifetime_evictions,
            checkout_not_ready_evictions: counters.checkout_not_ready_evictions,
            capacity_evictions: counters.capacity_evictions,
            idle_pool_entries,
            checked_out_pool_handles,
            hosts,
        }
    }

    /// Evict all idle connections for a pool key.
    ///
    /// Used after detecting a stale H2/H3 connection to ensure multiplexed
    /// clones sharing the same broken transport are not re-issued on retry.
    pub(crate) fn evict(&self, key: &PoolKey) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.idle.remove(key);
    }

    /// Returns true if there is an in-progress H2/H3 connection for this key.
    /// If so, returns true to let the caller wait and retry checkout.
    /// If not, marks the key as connecting and returns false.
    pub(crate) fn mark_connecting_h2(&self, key: &PoolKey) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner.connecting_h2.contains(key) {
            true
        } else {
            inner.connecting_h2.insert(key.clone());
            false
        }
    }

    /// Remove the connecting-in-progress mark for an H2/H3 key.
    pub(crate) fn unmark_connecting_h2(&self, key: &PoolKey) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.connecting_h2.remove(key);
        }
    }

    /// Find a coalesced connection: an idle h2/h3 connection whose SANs cover
    /// the target host and whose remote IP matches the resolved address.
    ///
    /// This enables connection coalescing per RFC 7540 §9.1.1.
    /// Uses a SAN→PoolKey reverse index for O(1) candidate lookup.
    pub(crate) fn checkout_coalesced(
        &self,
        target_host: &str,
        resolved_ip: Option<IpAddr>,
        proxy_route: ProxyRoute,
    ) -> Option<PooledConnection<B>> {
        let pool_weak = Arc::downgrade(&self.inner);
        let mut inner = self.inner.lock().ok()?;
        let now = Instant::now();
        let idle_timeout = inner.idle_timeout;
        let max_lifetime = inner.max_lifetime;
        let max_active = inner.max_active_streams_per_connection;

        let candidate_keys: Vec<PoolKey> =
            inner.san_index.get(target_host)?.iter().cloned().collect();

        let mut found_key = None;
        let mut found_conn = None;
        let mut active_key: Option<PoolKey> = None;

        // Scope: all queue operations happen here.
        {
            for key in &candidate_keys {
                if key.proxy_route != proxy_route {
                    continue;
                }
                let queue = match inner.idle.get_mut(key) {
                    Some(q) => q,
                    None => {
                        continue;
                    }
                };

                let mut i = queue.len();
                while i > 0 {
                    i -= 1;

                    if now.duration_since(queue[i].idle_since) >= idle_timeout {
                        continue;
                    }
                    if !Self::connection_within_lifetime(&queue[i].connection, max_lifetime) {
                        continue;
                    }
                    if !queue[i].connection.is_h2_or_h3() {
                        continue;
                    }
                    if !queue[i].connection.sans.iter().any(|s| s == target_host) {
                        continue;
                    }
                    if let Some(ip) = resolved_ip
                        && queue[i].connection.remote_addr.map(|a| a.ip()) != Some(ip)
                    {
                        continue;
                    }

                    if !queue[i].connection.is_ready() {
                        continue;
                    }

                    if queue[i].connection.is_h2_or_h3() {
                        if let Some(mut cloned) = queue[i]
                            .connection
                            .clone_for_multiplex_with_limit(max_active)
                        {
                            cloned.pool = pool_weak.clone();
                            cloned.key = Some(key.clone());
                            queue[i].connection.pool = Weak::new();
                            queue[i].connection.key = None;
                            queue[i].idle_since = now;
                            active_key = Some(key.clone());
                            found_conn = Some(cloned);
                            break;
                        }
                        continue;
                    }

                    if let Some(entry) = queue.remove(i) {
                        if queue.is_empty() {
                            found_key = Some(key.clone());
                        }
                        let mut conn = entry.connection;
                        conn.pool = pool_weak.clone();
                        conn.key = Some(key.clone());
                        active_key = Some(key.clone());
                        found_conn = Some(conn);
                        break;
                    }
                }
                if found_conn.is_some() {
                    break;
                }
            }
        }

        // Increment active count after queue borrows are released.
        if let Some(ref k) = active_key {
            if let Some(max) = inner.max_active_per_host {
                let active = inner.active.get(k).copied().unwrap_or(0);
                if active >= max.get() {
                    // Gate — put the connection back and return None.
                    #[allow(clippy::collapsible_if)]
                    if let Some(ref key) = found_key {
                        if let Some(queue) = inner.idle.get_mut(key) {
                            if let Some(conn) = found_conn.take() {
                                queue.push_back(IdleConnection {
                                    connection: conn,
                                    idle_since: Instant::now(),
                                });
                            }
                        }
                    }
                    return None;
                }
            }
            *inner.active.entry(k.clone()).or_insert(0) += 1;
        }

        if let Some(key) = found_key {
            inner.idle.remove(&key);
        }

        // Clean up stale index entries for keys that no longer have connections
        for key in &candidate_keys {
            if !inner.idle.contains_key(key)
                && let Some(keys) = inner.san_index.get_mut(target_host)
            {
                keys.remove(key);
                if keys.is_empty() {
                    inner.san_index.remove(target_host);
                }
            }
        }

        found_conn
    }

    pub(crate) fn ensure_reaper<R: RuntimePoll>(&self)
    where
        B: Send,
    {
        if !self.reaper_spawned.swap(true, Ordering::AcqRel) {
            self.spawn_reaper::<R>();
        }
    }

    fn spawn_reaper<R: RuntimePoll>(&self)
    where
        B: Send,
    {
        let inner = Arc::clone(&self.inner);
        let counters = Arc::clone(&self.counters);
        R::spawn_send(async move {
            loop {
                let timeout = {
                    let Ok(guard) = inner.lock() else {
                        return;
                    };
                    reaper_interval(guard.idle_timeout, guard.max_lifetime)
                };
                R::sleep(timeout).await;

                let Ok(mut guard) = inner.lock() else {
                    return;
                };
                let now = Instant::now();
                let idle_timeout = guard.idle_timeout;
                let max_lifetime = guard.max_lifetime;
                guard.idle.retain(|_, queue| {
                    queue.retain(|entry| {
                        if now.duration_since(entry.idle_since) >= idle_timeout {
                            counters
                                .idle_timeout_evictions
                                .fetch_add(1, Ordering::Relaxed);
                            return false;
                        }
                        if !Self::connection_within_lifetime(&entry.connection, max_lifetime) {
                            counters
                                .max_lifetime_evictions
                                .fetch_add(1, Ordering::Relaxed);
                            return false;
                        }
                        true
                    });
                    !queue.is_empty()
                });
                let live_keys: HashSet<PoolKey> = guard.idle.keys().cloned().collect();
                guard.san_index.retain(|_, keys| {
                    keys.retain(|k| live_keys.contains(k));
                    !keys.is_empty()
                });
            }
        });
    }

    /// Ensure the reaper is running on a local (single-threaded) runtime.
    pub(crate) fn ensure_reaper_local<R: crate::runtime::RuntimeLocal>(&self) {
        if !self.reaper_spawned.swap(true, Ordering::AcqRel) {
            self.spawn_reaper_local::<R>();
        }
    }

    fn spawn_reaper_local<R: crate::runtime::RuntimeLocal>(&self) {
        let inner = Arc::clone(&self.inner);
        let counters = Arc::clone(&self.counters);
        R::spawn_local(async move {
            loop {
                let timeout = {
                    let Ok(guard) = inner.lock() else {
                        return;
                    };
                    reaper_interval(guard.idle_timeout, guard.max_lifetime)
                };
                R::sleep(timeout).await;

                let Ok(mut guard) = inner.lock() else {
                    return;
                };
                let now = Instant::now();
                let idle_timeout = guard.idle_timeout;
                let max_lifetime = guard.max_lifetime;
                guard.idle.retain(|_, queue| {
                    queue.retain(|entry| {
                        if now.duration_since(entry.idle_since) >= idle_timeout {
                            counters
                                .idle_timeout_evictions
                                .fetch_add(1, Ordering::Relaxed);
                            return false;
                        }
                        if !Self::connection_within_lifetime(&entry.connection, max_lifetime) {
                            counters
                                .max_lifetime_evictions
                                .fetch_add(1, Ordering::Relaxed);
                            return false;
                        }
                        true
                    });
                    !queue.is_empty()
                });
                let live_keys: HashSet<PoolKey> = guard.idle.keys().cloned().collect();
                guard.san_index.retain(|_, keys| {
                    keys.retain(|k| live_keys.contains(k));
                    !keys.is_empty()
                });
            }
        });
    }
}

fn reaper_interval(idle_timeout: Duration, max_lifetime: Option<Duration>) -> Duration {
    match max_lifetime {
        Some(max_lifetime) if !max_lifetime.is_zero() => idle_timeout.min(max_lifetime),
        _ => idle_timeout,
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests_tokio;

#[cfg(all(test, feature = "smol"))]
mod tests_smol;

#[cfg(all(test, feature = "compio"))]
mod tests_compio;

#[cfg(test)]
mod tests_sync {
    use super::*;
    use crate::body::RequestBodySend;

    fn key(host: &str) -> PoolKey {
        PoolKey::new(
            Scheme::HTTP,
            host.parse::<Authority>().expect("valid authority"),
        )
    }

    /// When the mutex is poisoned, checkout should return None rather than panic.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn checkout_returns_none_on_poisoned_mutex() {
        let pool = ConnectionPool::<RequestBodySend>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));
        let k = key("example.com:80");

        // Poison the mutex by panicking inside a lock
        let inner = Arc::clone(&pool.inner);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.lock().unwrap();
            panic!("intentional panic to poison the mutex");
        }));
        assert!(result.is_err(), "panic should have occurred");

        // Now the mutex is poisoned. checkout should return None.
        let result = pool.checkout(&k);
        assert!(
            result.is_none(),
            "checkout on poisoned mutex should return None"
        );
    }

    /// When the mutex is poisoned, checkin should silently return.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn checkin_returns_on_poisoned_mutex() {
        let pool = ConnectionPool::<RequestBodySend>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));

        // Poison the mutex
        let inner = Arc::clone(&pool.inner);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.lock().unwrap();
            panic!("intentional panic to poison the mutex");
        }));

        // Verify the mutex is actually poisoned
        assert!(pool.inner.lock().is_err());

        // checkin should not panic even with a poisoned mutex.
        // We can't easily create a PooledConnection without a runtime handshake,
        // but we can verify that mark_connecting_h2 also handles it (tested below).
    }

    /// When the mutex is poisoned, mark_connecting_h2 should return false.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn mark_connecting_h2_returns_false_on_poisoned_mutex() {
        let pool = ConnectionPool::<RequestBodySend>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));
        let k = key("example.com:80");

        // Poison the mutex
        let inner = Arc::clone(&pool.inner);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.lock().unwrap();
            panic!("intentional panic to poison the mutex");
        }));

        assert!(pool.inner.lock().is_err(), "mutex should be poisoned");
        // mark_connecting_h2 should return false (not panic)
        assert!(!pool.mark_connecting_h2(&k));
    }

    /// When the mutex is poisoned, unmark_connecting_h2 should not panic.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn unmark_connecting_h2_no_panic_on_poisoned_mutex() {
        let pool = ConnectionPool::<RequestBodySend>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));
        let k = key("example.com:80");

        // Poison the mutex
        let inner = Arc::clone(&pool.inner);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.lock().unwrap();
            panic!("intentional panic to poison the mutex");
        }));

        assert!(pool.inner.lock().is_err(), "mutex should be poisoned");
        // Should not panic
        pool.unmark_connecting_h2(&k);
    }

    /// When the mutex is poisoned, checkout_coalesced should return None.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn checkout_coalesced_returns_none_on_poisoned_mutex() {
        let pool = ConnectionPool::<RequestBodySend>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));

        // Poison the mutex
        let inner = Arc::clone(&pool.inner);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.lock().unwrap();
            panic!("intentional panic to poison the mutex");
        }));

        assert!(pool.inner.lock().is_err(), "mutex should be poisoned");
        let ip: std::net::IpAddr = [10, 0, 0, 1].into();
        let result = pool.checkout_coalesced("example.com", Some(ip), ProxyRoute::DIRECT);
        assert!(
            result.is_none(),
            "checkout_coalesced on poisoned mutex should return None"
        );
    }
}
