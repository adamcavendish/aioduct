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
    let _pool = ConnectionPool::<TokioRuntime>::new_no_reaper(8, Duration::from_secs(30));
}

#[test]
fn pool_new_does_not_require_runtime() {
    let _pool = ConnectionPool::<TokioRuntime>::new(8, Duration::from_secs(30));
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

    for _ in 0..3 {
        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);
    }

    tokio::task::yield_now().await;

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

    tokio::task::yield_now().await;

    let out = pool.checkout(&k).expect("should get a connection");
    assert_eq!(
        out.remote_addr,
        Some(addr2),
        "LIFO: most recent connection first"
    );
}

#[tokio::test]
async fn checkout_expired_connection_returns_none() {
    let pool = ConnectionPool::<TokioRuntime>::new_no_reaper(8, Duration::from_millis(50));
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        pool.checkout(&k).is_none(),
        "expired connection should be discarded"
    );
}

#[tokio::test]
async fn reaper_removes_expired_connections() {
    let pool = ConnectionPool::<TokioRuntime>::new(1, Duration::from_millis(50));
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        pool.checkout(&k).is_none(),
        "reaper should have removed the expired connection"
    );
}
