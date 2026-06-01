/// Connection pool module with types for managing idle connections.
pub(crate) mod connection;

pub(crate) use connection::{HttpConnection, PooledConnection};

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Connection pool key identifying a (scheme, authority, protocol) triple.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) struct PoolKey {
    /// The URI scheme (http or https).
    pub(crate) scheme: Scheme,
    /// The URI authority (host and optional port).
    pub(crate) authority: Authority,
    /// Protocol hint for pool segregation.
    pub(crate) protocol: ProtocolHint,
}

impl PoolKey {
    /// Create a new pool key with the default protocol hint (Auto).
    #[allow(dead_code)]
    pub(crate) fn new(scheme: Scheme, authority: Authority) -> Self {
        Self {
            scheme,
            authority,
            protocol: ProtocolHint::Auto,
        }
    }

    /// Create a pool key that forces HTTP/2 prior knowledge.
    pub(crate) fn with_hint(scheme: Scheme, authority: Authority, protocol: ProtocolHint) -> Self {
        Self {
            scheme,
            authority,
            protocol,
        }
    }
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

/// Thread-safe pool of idle HTTP connections keyed by origin.
pub(crate) struct ConnectionPool<B> {
    inner: Arc<Mutex<PoolInner<B>>>,
    reaper_spawned: Arc<AtomicBool>,
}

impl<B> Clone for ConnectionPool<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            reaper_spawned: Arc::clone(&self.reaper_spawned),
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

    /// Returns whether a new connection can be created for the given key,
    /// respecting the configured `max_active_per_host` cap.
    #[allow(dead_code)]
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
    #[cfg(test)]
    #[allow(dead_code)]
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
        let pool_weak = Arc::downgrade(&self.inner);
        let mut inner = self.inner.lock().ok()?;
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
                    continue;
                }
                if !Self::connection_within_lifetime(&entry.connection, max_lifetime) {
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
        if let Some(ref k) = active_key
            && let Some(count) = inner.active.get_mut(k)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                inner.active.remove(k);
            }
        }

        let max = inner.max_idle_per_host;

        if max == 0 {
            return;
        }

        if !Self::connection_within_lifetime(&connection, inner.max_lifetime) {
            return;
        }

        // Clear pool/key so Drop does not double-decrement.
        connection.pool = Weak::new();
        connection.key = None;

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
        }
        queue.push_back(IdleConnection {
            connection,
            idle_since: Instant::now(),
        });
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
    ) -> Option<PooledConnection<B>> {
        let pool_weak = Arc::downgrade(&self.inner);
        let mut inner = self.inner.lock().ok()?;
        let now = Instant::now();
        let idle_timeout = inner.idle_timeout;
        let max_lifetime = inner.max_lifetime;
        let max_active = inner.max_active_streams_per_connection;

        let candidate_keys: Vec<PoolKey> = match inner.san_index.get(target_host) {
            Some(keys) => keys.iter().cloned().collect(),
            None => return None,
        };

        let mut found_key = None;
        let mut found_conn = None;
        let mut active_key: Option<PoolKey> = None;

        // Scope: all queue operations happen here.
        {
            for key in &candidate_keys {
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
                        now.duration_since(entry.idle_since) < idle_timeout
                            && Self::connection_within_lifetime(&entry.connection, max_lifetime)
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
                        now.duration_since(entry.idle_since) < idle_timeout
                            && Self::connection_within_lifetime(&entry.connection, max_lifetime)
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
        let result = pool.checkout_coalesced("example.com", Some(ip));
        assert!(
            result.is_none(),
            "checkout_coalesced on poisoned mutex should return None"
        );
    }
}
