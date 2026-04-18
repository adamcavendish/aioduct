pub mod connection;

pub use connection::{HttpConnection, PooledConnection};

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::uri::{Authority, Scheme};

use crate::runtime::Runtime;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct PoolKey {
    pub scheme: Scheme,
    pub authority: Authority,
}

impl PoolKey {
    pub fn new(scheme: Scheme, authority: Authority) -> Self {
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

pub struct ConnectionPool<R: Runtime> {
    inner: Arc<Mutex<PoolInner<R>>>,
}

impl<R: Runtime> Clone for ConnectionPool<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<R: Runtime> ConnectionPool<R> {
    pub fn new(max_idle_per_host: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner::<R> {
                idle: HashMap::new(),
                max_idle_per_host,
                idle_timeout,
                _runtime: PhantomData,
            })),
        }
    }

    pub fn checkout(&self, key: &PoolKey) -> Option<PooledConnection<R>> {
        let mut inner = self.inner.lock().unwrap();
        let idle_timeout = inner.idle_timeout;
        let queue = inner.idle.get_mut(key)?;
        let now = Instant::now();

        while let Some(entry) = queue.pop_front() {
            if now.duration_since(entry.idle_since) < idle_timeout {
                if queue.is_empty() {
                    inner.idle.remove(key);
                }
                return Some(entry.connection);
            }
        }

        inner.idle.remove(key);
        None
    }

    pub fn checkin(&self, key: PoolKey, connection: PooledConnection<R>) {
        let mut inner = self.inner.lock().unwrap();
        let max = inner.max_idle_per_host;
        let queue = inner.idle.entry(key).or_default();

        if queue.len() < max {
            queue.push_back(IdleConnection::<R> {
                connection,
                idle_since: Instant::now(),
                _runtime: PhantomData,
            });
        }
    }

    pub fn evict_expired(&self) {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        let timeout = inner.idle_timeout;

        inner.idle.retain(|_key, queue| {
            queue.retain(|entry| now.duration_since(entry.idle_since) < timeout);
            !queue.is_empty()
        });
    }
}
