/// Connection pool module with types for managing idle connections.
pub(crate) mod connection;

pub(crate) use connection::{HttpConnection, PooledConnection};

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::uri::{Authority, Scheme};

use crate::runtime::Runtime;

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

struct IdleConnection<R: Runtime> {
    connection: PooledConnection<R>,
    idle_since: Instant,
    _runtime: PhantomData<R>,
}

struct PoolInner<R: Runtime> {
    idle: HashMap<PoolKey, VecDeque<IdleConnection<R>>>,
    max_idle_per_host: usize,
    idle_timeout: Duration,
    _runtime: PhantomData<R>,
}

/// Thread-safe pool of idle HTTP connections keyed by origin.
pub(crate) struct ConnectionPool<R: Runtime> {
    inner: Arc<Mutex<PoolInner<R>>>,
    reaper_spawned: Arc<AtomicBool>,
}

impl<R: Runtime> Clone for ConnectionPool<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            reaper_spawned: Arc::clone(&self.reaper_spawned),
        }
    }
}

impl<R: Runtime> ConnectionPool<R> {
    /// Create a pool with the given capacity and timeout settings.
    pub(crate) fn new(max_idle_per_host: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner::<R> {
                idle: HashMap::new(),
                max_idle_per_host,
                idle_timeout,
                _runtime: PhantomData,
            })),
            reaper_spawned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a pool without spawning the background reaper task.
    ///
    /// This is useful for unit tests that don't need the reaper and may not
    /// have a full async runtime available.
    #[cfg(test)]
    pub(crate) fn new_no_reaper(max_idle_per_host: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner::<R> {
                idle: HashMap::new(),
                max_idle_per_host,
                idle_timeout,
                _runtime: PhantomData,
            })),
            reaper_spawned: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Retrieve an idle, ready connection for the given key.
    ///
    /// Uses LIFO ordering (most recently returned first) and checks readiness
    /// on each candidate, trying all pooled connections before giving up.
    pub(crate) fn checkout(&self, key: &PoolKey) -> Option<PooledConnection<R>> {
        let mut inner = self.inner.lock().unwrap();
        let idle_timeout = inner.idle_timeout;
        let queue = inner.idle.get_mut(key)?;
        let now = Instant::now();

        while let Some(entry) = queue.pop_back() {
            if now.duration_since(entry.idle_since) >= idle_timeout {
                continue;
            }
            if entry.connection.is_ready() {
                if queue.is_empty() {
                    inner.idle.remove(key);
                }
                return Some(entry.connection);
            }
        }

        inner.idle.remove(key);
        None
    }

    /// Return a connection to the pool for future reuse.
    ///
    /// When at capacity, evicts the oldest idle connection to make room.
    pub(crate) fn checkin(&self, key: PoolKey, connection: PooledConnection<R>) {
        self.ensure_reaper();
        let mut inner = self.inner.lock().unwrap();
        let max = inner.max_idle_per_host;
        let queue = inner.idle.entry(key).or_default();

        if queue.len() >= max {
            queue.pop_front();
        }
        queue.push_back(IdleConnection::<R> {
            connection,
            idle_since: Instant::now(),
            _runtime: PhantomData,
        });
    }

    /// Find a coalesced connection: an idle h2/h3 connection whose SANs cover
    /// the target host and whose remote IP matches the resolved address.
    ///
    /// This enables connection coalescing per RFC 7540 §9.1.1.
    pub(crate) fn checkout_coalesced(
        &self,
        target_host: &str,
        resolved_ip: Option<IpAddr>,
    ) -> Option<PooledConnection<R>> {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        let idle_timeout = inner.idle_timeout;

        let mut found_key = None;
        let mut found_conn = None;

        for (key, queue) in inner.idle.iter_mut() {
            let mut i = queue.len();
            while i > 0 {
                i -= 1;
                let entry = &queue[i];

                if now.duration_since(entry.idle_since) >= idle_timeout {
                    continue;
                }
                if !entry.connection.is_h2_or_h3() {
                    continue;
                }
                if !entry.connection.sans.iter().any(|s| s == target_host) {
                    continue;
                }
                if let Some(ip) = resolved_ip {
                    if entry.connection.remote_addr.map(|a| a.ip()) != Some(ip) {
                        continue;
                    }
                }

                let entry = queue.remove(i).unwrap();
                if entry.connection.is_ready() {
                    if queue.is_empty() {
                        found_key = Some(key.clone());
                    }
                    found_conn = Some(entry.connection);
                    break;
                }
            }
            if found_conn.is_some() {
                break;
            }
        }

        if let Some(key) = found_key {
            inner.idle.remove(&key);
        }
        found_conn
    }

    fn ensure_reaper(&self) {
        if !self.reaper_spawned.swap(true, Ordering::AcqRel) {
            self.spawn_reaper();
        }
    }

    fn spawn_reaper(&self) {
        let inner = Arc::clone(&self.inner);
        R::spawn(async move {
            loop {
                let timeout = {
                    let guard = inner.lock().unwrap();
                    guard.idle_timeout
                };
                R::sleep(timeout).await;

                let mut guard = inner.lock().unwrap();
                let now = Instant::now();
                let idle_timeout = guard.idle_timeout;
                guard.idle.retain(|_, queue| {
                    queue.retain(|entry| now.duration_since(entry.idle_since) < idle_timeout);
                    !queue.is_empty()
                });
            }
        });
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests_tokio;

#[cfg(all(test, feature = "smol"))]
mod tests_smol;

#[cfg(all(test, feature = "compio"))]
mod tests_compio;
