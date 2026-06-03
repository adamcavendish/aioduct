use super::*;
use crate::body::RequestBodyLocal;
use crate::runtime::CompioRuntime;
use crate::runtime::compio_rt::CompioIo;
use crate::runtime::{RuntimeCompletion, RuntimeLocal};

async fn make_h1_conn() -> PooledConnection<RequestBodyLocal> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let async_listener = async_io::Async::new(listener).unwrap();

    let client_tcp = async_io::Async::<std::net::TcpStream>::connect(addr)
        .await
        .unwrap();
    let (server_tcp, _) = async_listener.accept().await.unwrap();

    // Keep server socket alive — drain reads in a background task.
    // Must stay as async_io::Async (not into_inner) so the reactor keeps the fd registered.
    compio_runtime::spawn(async move {
        use futures_io::AsyncRead;
        let mut server = server_tcp;
        let mut buf = [0u8; 1024];
        while std::future::poll_fn(|cx| std::pin::Pin::new(&mut server).poll_read(cx, &mut buf))
            .await
            .unwrap_or(0)
            > 0
        {}
    })
    .detach();

    let io = CompioIo::new(client_tcp);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake should succeed");

    CompioRuntime::spawn_local(async move {
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

/// Wait for async-io's background reactor to drive the connection driver,
/// yielding multiple times so the cross-reactor wakeup has time to land.
async fn wait_for_ready(pool: &ConnectionPool<RequestBodyLocal>, k: &PoolKey) -> bool {
    for _ in 0..10 {
        CompioRuntime::sleep(Duration::from_millis(5)).await;
        let inner = pool.inner.lock().unwrap();
        if let Some(queue) = inner.idle.get(k)
            && queue.back().is_some_and(|e| e.connection.is_ready())
        {
            return true;
        }
    }
    false
}

#[test]
fn checkout_returns_none_on_empty_pool() {
    let pool = ConnectionPool::<RequestBodyLocal>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    assert!(pool.checkout(&key("example.com:80")).is_none());
}

#[test]
fn checkin_then_checkout_returns_connection() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        assert!(
            wait_for_ready(&pool, &k).await,
            "connection should become ready"
        );

        let out = pool.checkout(&k);
        assert!(
            out.is_some(),
            "checkout should return the checked-in connection"
        );
    });
}

#[test]
fn checkout_with_different_key_returns_none() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));

        let conn = make_h1_conn().await;
        pool.checkin(key("a.example.com:80"), conn);

        assert!(
            wait_for_ready(&pool, &key("a.example.com:80")).await,
            "connection should become ready"
        );

        assert!(
            pool.checkout(&key("b.example.com:80")).is_none(),
            "checkout with a different key should return None"
        );
    });
}

#[test]
fn checkin_checkout_is_lifo() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30));
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

        assert!(
            wait_for_ready(&pool, &k).await,
            "connections should become ready"
        );

        let out = pool.checkout(&k).expect("should get a connection");
        assert_eq!(
            out.remote_addr,
            Some(addr2),
            "LIFO: most recent connection first"
        );
    });
}

#[test]
fn pool_respects_max_idle_per_host() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let max_idle = 2;
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(max_idle)
            .with_idle_timeout(Duration::from_secs(30));
        let k = key("example.com:80");

        for _ in 0..3 {
            let conn = make_h1_conn().await;
            pool.checkin(k.clone(), conn);
        }

        assert!(
            wait_for_ready(&pool, &k).await,
            "connections should become ready"
        );

        assert!(pool.checkout(&k).is_some(), "1st checkout should succeed");
        assert!(pool.checkout(&k).is_some(), "2nd checkout should succeed");
        assert!(
            pool.checkout(&k).is_none(),
            "3rd checkout should return None (capacity was 2)"
        );
    });
}

#[test]
fn checkout_expired_connection_returns_none() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_millis(50));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        CompioRuntime::sleep(Duration::from_millis(100)).await;

        assert!(
            pool.checkout(&k).is_none(),
            "expired connection should be discarded"
        );
    });
}

#[test]
fn idle_timeout_eviction_on_checkout() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_millis(1));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        CompioRuntime::sleep(Duration::from_millis(50)).await;

        assert!(
            pool.checkout(&k).is_none(),
            "expired connection should be discarded on checkout"
        );
    });
}

// Note: no reaper test for compio — CompioRuntime is completion-based and does
// not implement RuntimePoll, which is required by ConnectionPool::ensure_reaper.

// --- max_active_per_host tests ---

#[test]
fn max_active_per_host_blocks_when_at_cap() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let max_active = std::num::NonZeroUsize::new(1).unwrap();
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30))
            .with_max_active_per_host(Some(max_active));
        let k = key("example.com:80");

        assert!(
            pool.can_connect(&k),
            "can_connect should return true initially"
        );

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        assert!(
            wait_for_ready(&pool, &k).await,
            "connection should become ready"
        );

        let _out = pool.checkout(&k).expect("first checkout should succeed");
        assert!(
            !pool.can_connect(&k),
            "can_connect should return false when at cap"
        );
    });
}

#[test]
fn checkin_frees_active_slot() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let max_active = std::num::NonZeroUsize::new(1).unwrap();
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30))
            .with_max_active_per_host(Some(max_active));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        assert!(
            wait_for_ready(&pool, &k).await,
            "connection should become ready"
        );

        let out = pool.checkout(&k).expect("first checkout");
        assert!(!pool.can_connect(&k), "at cap after checkout");

        pool.checkin(k.clone(), out);
        assert!(
            pool.can_connect(&k),
            "can_connect should return true after checkin frees the slot"
        );

        assert!(
            wait_for_ready(&pool, &k).await,
            "connection should become ready after checkin"
        );

        let out2 = pool.checkout(&k);
        assert!(out2.is_some(), "should checkout after checkin freed slot");
    });
}

#[test]
fn drop_frees_active_slot() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let max_active = std::num::NonZeroUsize::new(1).unwrap();
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30))
            .with_max_active_per_host(Some(max_active));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        assert!(
            wait_for_ready(&pool, &k).await,
            "connection should become ready"
        );

        let out = pool.checkout(&k).expect("first checkout");
        assert!(!pool.can_connect(&k), "at cap after checkout");

        drop(out);
        assert!(
            pool.can_connect(&k),
            "can_connect should return true after drop frees the slot"
        );
    });
}

#[test]
fn max_active_per_host_none_means_unlimited() {
    let pool = ConnectionPool::<RequestBodyLocal>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");
    assert!(pool.can_connect(&k));
}

#[test]
fn max_active_per_host_zero_disables_cap() {
    let pool = ConnectionPool::<RequestBodyLocal>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_per_host(None);
    let k = key("example.com:80");
    assert!(pool.can_connect(&k));
}

#[test]
fn per_host_isolation() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let max_active = std::num::NonZeroUsize::new(1).unwrap();
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30))
            .with_max_active_per_host(Some(max_active));
        let k1 = key("a.example.com:80");
        let k2 = key("b.example.com:80");

        assert!(pool.can_connect(&k1));
        assert!(pool.can_connect(&k2));

        let conn1 = make_h1_conn().await;
        pool.checkin(k1.clone(), conn1);

        assert!(
            wait_for_ready(&pool, &k1).await,
            "connection should become ready"
        );

        let _out1 = pool.checkout(&k1).expect("checkout k1");

        assert!(!pool.can_connect(&k1), "k1 should be at cap");
        assert!(
            pool.can_connect(&k2),
            "k2 should not be affected by k1's cap"
        );
    });
}

#[test]
fn max_active_per_host_isolation() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let max_active = std::num::NonZeroUsize::new(1).unwrap();
        let pool = ConnectionPool::<RequestBodyLocal>::new()
            .without_reaper()
            .with_max_idle_per_host(8)
            .with_idle_timeout(Duration::from_secs(30))
            .with_max_active_per_host(Some(max_active));
        let k_a = key("a.example.com:80");
        let k_b = key("b.example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k_a.clone(), conn);

        assert!(
            wait_for_ready(&pool, &k_a).await,
            "connection should become ready"
        );

        let _out = pool.checkout(&k_a).expect("checkout host A");
        assert!(
            !pool.can_connect(&k_a),
            "can_connect should return false for host A at cap"
        );
        assert!(
            pool.can_connect(&k_b),
            "can_connect should return true for host B"
        );
    });
}
