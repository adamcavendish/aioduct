/// Connection pool module with types for managing idle connections.
pub(crate) mod connection;

pub(crate) use connection::{HttpConnection, PooledConnection};

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
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
}

impl<R: Runtime> Clone for ConnectionPool<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<R: Runtime> ConnectionPool<R> {
    /// Create a pool with the given capacity and timeout settings.
    pub(crate) fn new(max_idle_per_host: usize, idle_timeout: Duration) -> Self {
        let pool = Self {
            inner: Arc::new(Mutex::new(PoolInner::<R> {
                idle: HashMap::new(),
                max_idle_per_host,
                idle_timeout,
                _runtime: PhantomData,
            })),
        };
        pool.spawn_reaper();
        pool
    }

    /// Create a pool without spawning the background reaper task.
    ///
    /// This is useful for unit tests that don't need the reaper and may not
    /// have a full async runtime available.
    #[cfg(all(test, feature = "tokio"))]
    pub(crate) fn new_no_reaper(max_idle_per_host: usize, idle_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner::<R> {
                idle: HashMap::new(),
                max_idle_per_host,
                idle_timeout,
                _runtime: PhantomData,
            })),
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
mod tests {
    use super::*;
    use crate::runtime::TokioRuntime;
    use crate::runtime::tokio_rt::TokioIo;

    /// Helper: perform an HTTP/1.1 handshake over a duplex stream and return
    /// the resulting `PooledConnection<TokioRuntime>`.
    async fn make_h1_conn() -> PooledConnection<TokioRuntime> {
        let (client_io, mut server_io) = tokio::io::duplex(1024);

        // Spawn a task that reads from the server side so the connection stays
        // alive and the sender reports `is_ready() == true`.
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 1024];
            loop {
                match server_io.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
            }
        });

        let io = TokioIo::new(client_io);
        let (sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .expect("h1 handshake should succeed on duplex");

        // Drive the connection in the background.
        tokio::spawn(async move {
            let _ = conn.await;
        });

        PooledConnection::new_h1(sender)
    }

    fn key(host: &str) -> PoolKey {
        PoolKey::new(
            Scheme::HTTP,
            host.parse::<Authority>().expect("valid authority"),
        )
    }

    #[test]
    fn pool_creates_with_given_parameters() {
        // The pool can be constructed without panicking.
        let _pool = ConnectionPool::<TokioRuntime>::new_no_reaper(8, Duration::from_secs(30));
    }

    #[test]
    fn checkout_returns_none_on_empty_pool() {
        let pool = ConnectionPool::<TokioRuntime>::new_no_reaper(8, Duration::from_secs(30));
        assert!(pool.checkout(&key("example.com:80")).is_none());
    }

    #[tokio::test]
    async fn checkin_then_checkout_returns_connection() {
        let pool = ConnectionPool::<TokioRuntime>::new_no_reaper(8, Duration::from_secs(30));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        // Yield so the background connection driver task can run and make
        // the sender ready.
        tokio::task::yield_now().await;

        let out = pool.checkout(&k);
        assert!(
            out.is_some(),
            "checkout should return the checked-in connection"
        );
    }

    #[tokio::test]
    async fn checkout_with_different_key_returns_none() {
        let pool = ConnectionPool::<TokioRuntime>::new_no_reaper(8, Duration::from_secs(30));

        let conn = make_h1_conn().await;
        pool.checkin(key("a.example.com:80"), conn);

        tokio::task::yield_now().await;

        assert!(
            pool.checkout(&key("b.example.com:80")).is_none(),
            "checkout with a different key should return None"
        );
    }

    #[tokio::test]
    async fn pool_respects_max_idle_per_host() {
        let max_idle = 2;
        let pool = ConnectionPool::<TokioRuntime>::new_no_reaper(max_idle, Duration::from_secs(30));
        let k = key("example.com:80");

        // Check in 3 connections; the pool should only keep `max_idle` (2).
        for _ in 0..3 {
            let conn = make_h1_conn().await;
            pool.checkin(k.clone(), conn);
        }

        // Yield so background connection driver tasks can run.
        tokio::task::yield_now().await;

        // We should be able to check out exactly 2, then get None.
        assert!(pool.checkout(&k).is_some(), "1st checkout should succeed");
        assert!(pool.checkout(&k).is_some(), "2nd checkout should succeed");
        assert!(
            pool.checkout(&k).is_none(),
            "3rd checkout should return None (capacity was 2)"
        );
    }

    #[tokio::test]
    async fn checkin_checkout_is_lifo() {
        let pool = ConnectionPool::<TokioRuntime>::new_no_reaper(8, Duration::from_secs(30));
        let k = key("example.com:80");

        let conn1 = make_h1_conn().await;
        let addr1 = std::net::SocketAddr::from(([1, 1, 1, 1], 80));
        let mut conn1 = conn1;
        conn1.remote_addr = Some(addr1);
        pool.checkin(k.clone(), conn1);

        let conn2 = make_h1_conn().await;
        let addr2 = std::net::SocketAddr::from(([2, 2, 2, 2], 80));
        let mut conn2 = conn2;
        conn2.remote_addr = Some(addr2);
        pool.checkin(k.clone(), conn2);

        // Yield so background connection driver tasks can run.
        tokio::task::yield_now().await;

        // LIFO: the most recently checked-in (conn2) should come out first.
        let out = pool.checkout(&k).expect("should get a connection");
        assert_eq!(
            out.remote_addr,
            Some(addr2),
            "LIFO: most recent connection first"
        );
    }
}
