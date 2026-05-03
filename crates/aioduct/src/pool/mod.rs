/// Connection pool module with types for managing idle connections.
pub(crate) mod connection;

pub(crate) use connection::{HttpConnection, PooledConnection};

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::uri::{Authority, Scheme};

use crate::runtime::Runtime;

/// Connection pool key identifying a (scheme, authority) pair.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) struct PoolKey {
    /// The URI scheme (http or https).
    pub(crate) scheme: Scheme,
    /// The URI authority (host and optional port).
    pub(crate) authority: Authority,
}

impl PoolKey {
    /// Create a new pool key.
    pub(crate) fn new(scheme: Scheme, authority: Authority) -> Self {
        Self { scheme, authority }
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
