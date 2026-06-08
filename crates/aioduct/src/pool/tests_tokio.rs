use super::*;
use crate::body::RequestBodySend;
use crate::runtime::TokioRuntime;
use crate::runtime::tokio_rt::TokioIo;

/// Helper: perform an HTTP/1.1 handshake over a duplex stream and return
/// the resulting `PooledConnection`.
async fn make_h1_conn() -> PooledConnection<RequestBodySend> {
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
fn checkout_returns_none_on_empty_pool() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    assert!(pool.checkout(&key("example.com:80")).is_none());
}

#[tokio::test]
async fn checkin_then_checkout_returns_connection() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
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
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));

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
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(max_idle)
        .with_idle_timeout(Duration::from_secs(30));
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
    let pool = ConnectionPool::<RequestBodySend>::new()
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
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_millis(50));
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
async fn checkout_connection_past_max_lifetime_returns_none() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_lifetime(Duration::from_millis(50));
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        pool.checkout(&k).is_none(),
        "connection older than max lifetime should be discarded"
    );
}

#[tokio::test]
async fn checkin_connection_past_max_lifetime_drops_it() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_lifetime(Duration::ZERO);
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    assert!(
        pool.checkout(&k).is_none(),
        "expired connection should not be inserted into the pool"
    );
}

#[tokio::test]
async fn reaper_removes_expired_connections() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .with_max_idle_per_host(1)
        .with_idle_timeout(Duration::from_millis(50));
    pool.ensure_reaper::<TokioRuntime>();
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        pool.checkout(&k).is_none(),
        "reaper should have removed the expired connection"
    );
}

#[tokio::test]
async fn reaper_removes_connections_past_max_lifetime() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .with_max_idle_per_host(1)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_lifetime(Duration::from_millis(50));
    pool.ensure_reaper::<TokioRuntime>();
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        pool.checkout(&k).is_none(),
        "reaper should remove connections older than max lifetime"
    );
}

// --- Connection coalescing tests ---

use std::net::IpAddr;

/// Helper: perform an HTTP/2 handshake over a duplex stream.
async fn make_h2_conn() -> PooledConnection<RequestBodySend> {
    let (client_io, server_io) = tokio::io::duplex(65536);

    // Spawn server-side h2 connection handler
    tokio::spawn(async move {
        use hyper::server::conn::http2::Builder;
        use hyper::service::service_fn;
        let io = TokioIo::new(server_io);
        let _ = Builder::new(crate::runtime::executor::poll_executor::<TokioRuntime>())
            .serve_connection(
                io,
                service_fn(|_req| async {
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        http_body_util::Empty::<bytes::Bytes>::new(),
                    ))
                }),
            )
            .await;
    });

    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http2::handshake(
        crate::runtime::executor::poll_executor::<TokioRuntime>(),
        io,
    )
    .await
    .expect("h2 handshake should succeed on duplex");

    tokio::spawn(async move {
        let _ = conn.await;
    });

    PooledConnection::new_h2(sender)
}

fn key_https(host: &str) -> PoolKey {
    PoolKey::new(
        Scheme::HTTPS,
        host.parse::<Authority>().expect("valid authority"),
    )
}

#[tokio::test]
async fn checkout_coalesced_finds_by_san() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "cdn.example.com".into(),
        "api.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(result.is_some(), "should find coalesced connection via SAN");
}

#[tokio::test]
async fn checkout_coalesced_rejects_h1() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h1_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(result.is_none(), "h1 connections should not be coalesced");
}

#[tokio::test]
async fn checkout_coalesced_rejects_different_ip() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    let different_ip: IpAddr = [10, 0, 0, 2].into();
    let result = pool.checkout_coalesced("cdn.example.com", Some(different_ip));
    assert!(result.is_none(), "different IP should prevent coalescing");
}

#[tokio::test]
async fn checkout_coalesced_skips_expired() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_millis(50));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::time::sleep(Duration::from_millis(100)).await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(
        result.is_none(),
        "expired connection should not be returned"
    );
}

#[test]
fn checkout_coalesced_empty_pool_returns_none() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(result.is_none(), "empty pool should return None");
}

#[tokio::test]
async fn coalesced_checkout_skips_past_max_lifetime() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_lifetime(Duration::from_millis(1));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(
        result.is_none(),
        "connection past max lifetime should not be returned"
    );
}

#[test]
fn mark_connecting_h2_returns_false_first_time() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");
    assert!(!pool.mark_connecting_h2(&k));
}

#[test]
fn mark_connecting_h2_returns_true_when_already_present() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");
    assert!(!pool.mark_connecting_h2(&k));
    assert!(pool.mark_connecting_h2(&k));
}

#[test]
fn unmark_connecting_h2_allows_re_mark() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");
    assert!(!pool.mark_connecting_h2(&k));
    pool.unmark_connecting_h2(&k);
    assert!(!pool.mark_connecting_h2(&k));
}

#[test]
fn pool_key_new_vs_with_hint_default() {
    let k1 = PoolKey::new(Scheme::HTTP, "example.com:80".parse().unwrap());
    let k2 = PoolKey::with_hint(
        Scheme::HTTP,
        "example.com:80".parse().unwrap(),
        ProtocolHint::Auto,
    );
    assert_eq!(k1, k2);
}

#[test]
fn pool_key_different_hints_not_equal() {
    let k1 = PoolKey::with_hint(
        Scheme::HTTP,
        "example.com:80".parse().unwrap(),
        ProtocolHint::Auto,
    );
    let k2 = PoolKey::with_hint(
        Scheme::HTTP,
        "example.com:80".parse().unwrap(),
        ProtocolHint::H2c,
    );
    assert_ne!(k1, k2);
}

#[test]
fn protocol_hint_debug() {
    assert_eq!(format!("{:?}", ProtocolHint::Auto), "Auto");
    assert_eq!(format!("{:?}", ProtocolHint::H2c), "H2c");
    assert_eq!(format!("{:?}", ProtocolHint::AdaptiveH2c), "AdaptiveH2c");
}

#[tokio::test]
async fn checkout_h2_clone_for_multiplex() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("example.com:443");

    let conn = make_h2_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    let out1 = pool.checkout(&k);
    assert!(out1.is_some(), "first checkout should succeed");

    let out2 = pool.checkout(&k);
    assert!(
        out2.is_some(),
        "second checkout should succeed (H2 multiplex clone)"
    );
}

#[tokio::test]
async fn checkout_h2_respects_max_active_streams_per_connection() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_streams_per_connection(std::num::NonZeroUsize::new(1).unwrap());
    let k = key_https("example.com:443");

    let conn = make_h2_conn().await;
    pool.checkin(k.clone(), conn);

    let out1 = pool.checkout(&k).expect("first active stream");
    assert!(
        pool.checkout(&k).is_none(),
        "second checkout should hit this connection's active stream limit"
    );

    drop(out1);
    assert!(
        pool.checkout(&k).is_some(),
        "dropping the active clone should release stream capacity"
    );
}

#[tokio::test]
async fn checkout_h2_active_stream_limit_is_per_connection() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_streams_per_connection(std::num::NonZeroUsize::new(1).unwrap());
    let k = key_https("example.com:443");

    pool.checkin(k.clone(), make_h2_conn().await);
    pool.checkin(k.clone(), make_h2_conn().await);

    let _out1 = pool.checkout(&k).expect("first connection stream");
    let _out2 = pool.checkout(&k).expect("second connection stream");
}

#[tokio::test]
async fn max_active_streams_per_connection_does_not_change_h1_checkout() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_streams_per_connection(std::num::NonZeroUsize::new(1).unwrap());
    let k = key("example.com:80");

    pool.checkin(k.clone(), make_h1_conn().await);
    pool.checkin(k.clone(), make_h1_conn().await);

    tokio::task::yield_now().await;

    assert!(pool.checkout(&k).is_some(), "first h1 checkout");
    assert!(pool.checkout(&k).is_some(), "second h1 checkout");
}

#[tokio::test]
async fn checkout_coalesced_respects_max_active_streams_per_connection() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_streams_per_connection(std::num::NonZeroUsize::new(1).unwrap());
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    let ip: IpAddr = [10, 0, 0, 1].into();
    let out1 = pool
        .checkout_coalesced("cdn.example.com", Some(ip))
        .expect("first coalesced stream");
    assert!(
        pool.checkout_coalesced("cdn.example.com", Some(ip))
            .is_none(),
        "second coalesced checkout should hit this connection's active stream limit"
    );

    drop(out1);
    assert!(
        pool.checkout_coalesced("cdn.example.com", Some(ip))
            .is_some(),
        "dropping the active clone should release coalesced stream capacity"
    );
}

#[tokio::test]
async fn checkout_coalesced_without_ip_check() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    let result = pool.checkout_coalesced("cdn.example.com", None);
    assert!(
        result.is_some(),
        "should find coalesced connection when resolved_ip is None"
    );
}

#[tokio::test]
async fn checkout_coalesced_san_not_found() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("unknown.example.com", Some(ip));
    assert!(result.is_none(), "SAN not in cert should return None");
}

// --- Reaper task body tests ---

#[tokio::test]
async fn reaper_cleans_san_index_for_expired_connections() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .with_max_idle_per_host(1)
        .with_idle_timeout(Duration::from_millis(50));
    pool.ensure_reaper::<TokioRuntime>();
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    // Wait for idle timeout + reaper cycle
    tokio::time::sleep(Duration::from_millis(150)).await;

    // SAN index should be cleaned up, so coalesced lookup should fail
    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(
        result.is_none(),
        "reaper should have cleaned expired connections and SAN index"
    );
}

#[tokio::test]
async fn reaper_retains_live_connections() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .with_max_idle_per_host(4)
        .with_idle_timeout(Duration::from_secs(10));
    pool.ensure_reaper::<TokioRuntime>();
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    // Sleep less than the idle timeout but enough for reaper to fire
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connection should still be available
    let result = pool.checkout(&k);
    assert!(
        result.is_some(),
        "reaper should retain connections that haven't expired"
    );
}

// --- PooledConnection fields/methods tests ---

#[tokio::test]
async fn pooled_connection_new_h1_defaults() {
    let conn = make_h1_conn().await;
    assert!(conn.remote_addr.is_none());
    assert!(conn.tls_info.is_none());
    assert!(conn.tls_handshake_duration.is_none());
    assert!(conn.sans.is_empty());
    assert_eq!(conn.requests_served(), 0);
    assert_eq!(conn.bytes_sent(), 0);
    assert_eq!(conn.bytes_received(), 0);
    assert!(!conn.is_multiplex_clone);
    assert!(!conn.is_h2_or_h3());
    // Note: is_ready() for h1 depends on the background connection driver timing;
    // tested transitively via checkin_then_checkout_returns_connection.
}

#[tokio::test]
async fn pooled_connection_new_h2_defaults() {
    let conn = make_h2_conn().await;
    assert!(conn.remote_addr.is_none());
    assert!(conn.tls_info.is_none());
    assert!(conn.tls_handshake_duration.is_none());
    assert!(conn.sans.is_empty());
    assert_eq!(conn.requests_served(), 0);
    assert_eq!(conn.bytes_sent(), 0);
    assert_eq!(conn.bytes_received(), 0);
    assert!(!conn.is_multiplex_clone);
    assert!(conn.is_h2_or_h3());
    assert!(conn.is_ready());
}

#[tokio::test]
async fn clone_for_multiplex_returns_none_for_h1() {
    let conn = make_h1_conn().await;
    assert!(conn.clone_for_multiplex().is_none());
}

#[tokio::test]
async fn clone_for_multiplex_returns_some_for_h2() {
    let mut conn = make_h2_conn().await;
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    conn.record_request(1024);
    // Record two more requests to reach 5 total
    conn.record_request(0);
    conn.record_request(0);
    conn.record_request(0);
    conn.record_request(0);
    conn.record_bytes_received(4096);

    let cloned = conn.clone_for_multiplex();
    assert!(cloned.is_some());

    let cloned = cloned.unwrap();
    assert!(cloned.is_multiplex_clone);
    assert_eq!(
        cloned.remote_addr,
        Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)))
    );
    // Cloned handle shares transport-cumulative metrics
    assert_eq!(cloned.requests_served(), 5);
    assert_eq!(cloned.bytes_sent(), 1024);
    assert_eq!(cloned.bytes_received(), 4096);
    assert!(cloned.is_h2_or_h3());
}

#[tokio::test]
async fn multiplex_clone_releases_active_stream_count_on_drop() {
    let conn = make_h2_conn().await;
    assert_eq!(conn.active_multiplex_streams(), Some(0));

    let cloned = conn.clone_for_multiplex().expect("h2 should clone");
    assert_eq!(conn.active_multiplex_streams(), Some(1));

    drop(cloned);
    assert_eq!(conn.active_multiplex_streams(), Some(0));
}

// --- PoolKey Debug tests ---

#[test]
fn pool_key_debug_format() {
    let k = PoolKey::new(Scheme::HTTPS, "example.com:443".parse().unwrap());
    let debug = format!("{:?}", k);
    assert!(debug.contains("example.com:443"));
    assert!(debug.contains("Auto"));
}

#[test]
fn pool_key_with_hint_h2c_debug() {
    let k = PoolKey::with_hint(
        Scheme::HTTP,
        "example.com:80".parse().unwrap(),
        ProtocolHint::H2c,
    );
    let debug = format!("{:?}", k);
    assert!(debug.contains("H2c"));
}

#[test]
fn pool_key_with_hint_adaptive_debug() {
    let k = PoolKey::with_hint(
        Scheme::HTTP,
        "example.com:80".parse().unwrap(),
        ProtocolHint::AdaptiveH2c,
    );
    let debug = format!("{:?}", k);
    assert!(debug.contains("AdaptiveH2c"));
}

// --- ConnectionPool clone test ---

#[tokio::test]
async fn pool_clone_shares_state() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let pool2 = pool.clone();
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    // Checkout from clone should see the connection
    let out = pool2.checkout(&k);
    assert!(out.is_some(), "cloned pool should share internal state");
}

// --- Checkin eviction test ---

#[tokio::test]
async fn checkin_evicts_oldest_when_at_capacity() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(1)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");

    let mut conn1 = make_h1_conn().await;
    conn1.remote_addr = Some(std::net::SocketAddr::from(([1, 1, 1, 1], 80)));
    pool.checkin(k.clone(), conn1);

    let mut conn2 = make_h1_conn().await;
    conn2.remote_addr = Some(std::net::SocketAddr::from(([2, 2, 2, 2], 80)));
    pool.checkin(k.clone(), conn2);

    tokio::task::yield_now().await;

    // Only the second connection should remain (capacity is 1)
    let out = pool.checkout(&k).expect("should have a connection");
    assert_eq!(
        out.remote_addr,
        Some(std::net::SocketAddr::from(([2, 2, 2, 2], 80))),
        "should get the newer connection after eviction"
    );
    assert!(
        pool.checkout(&k).is_none(),
        "only one connection should remain"
    );
}

// --- SAN index population on checkin ---

#[tokio::test]
async fn checkin_populates_san_index() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "alt.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    // Should find via SAN index
    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("alt.example.com", Some(ip));
    assert!(result.is_some(), "SAN index should be populated on checkin");
}

// --- checkout_coalesced with IP address in SAN ---

#[tokio::test]
async fn checkout_coalesced_multiple_sans_finds_any() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "first.example.com".into(),
        "second.example.com".into(),
        "third.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    // Should find for any SAN
    assert!(
        pool.checkout_coalesced("first.example.com", Some(ip))
            .is_some()
    );
    assert!(
        pool.checkout_coalesced("second.example.com", Some(ip))
            .is_some()
    );
    assert!(
        pool.checkout_coalesced("third.example.com", Some(ip))
            .is_some()
    );
}

// --- mark/unmark connecting_h2 with different keys ---

#[test]
fn mark_connecting_h2_independent_keys() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k1 = key("a.example.com:80");
    let k2 = key("b.example.com:80");

    assert!(!pool.mark_connecting_h2(&k1));
    assert!(!pool.mark_connecting_h2(&k2));
    // k1 is already marked
    assert!(pool.mark_connecting_h2(&k1));
    // k2 is also already marked
    assert!(pool.mark_connecting_h2(&k2));

    pool.unmark_connecting_h2(&k1);
    assert!(!pool.mark_connecting_h2(&k1));
    // k2 is still marked
    assert!(pool.mark_connecting_h2(&k2));
}

// --- checkout_coalesced cleans stale san_index ---

#[tokio::test]
async fn checkout_coalesced_cleans_stale_san_index() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_millis(50));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "stale.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    // Wait for expiration
    tokio::time::sleep(Duration::from_millis(100)).await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    // This triggers cleanup of stale index entries
    let result = pool.checkout_coalesced("stale.example.com", Some(ip));
    assert!(
        result.is_none(),
        "expired connection should not be returned"
    );

    // After the cleanup, a subsequent lookup should also return None quickly
    let result2 = pool.checkout_coalesced("stale.example.com", Some(ip));
    assert!(result2.is_none());
}

// --- checkout_coalesced with not-ready connection (covers lines 260-270, 278, 284-289) ---

/// Creates an H2 connection and a channel to shut down the server side.
async fn make_h2_conn_with_shutdown() -> (
    PooledConnection<RequestBodySend>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (client_io, server_io) = tokio::io::duplex(65536);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Spawn server-side h2 connection handler that stops on shutdown signal
    tokio::spawn(async move {
        use hyper::server::conn::http2::Builder;
        use hyper::service::service_fn;
        let io = TokioIo::new(server_io);
        let conn = Builder::new(crate::runtime::executor::poll_executor::<TokioRuntime>())
            .serve_connection(
                io,
                service_fn(|_req| async {
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        http_body_util::Empty::<bytes::Bytes>::new(),
                    ))
                }),
            );
        tokio::select! {
            _ = conn => {},
            _ = shutdown_rx => {},
        }
    });

    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http2::handshake(
        crate::runtime::executor::poll_executor::<TokioRuntime>(),
        io,
    )
    .await
    .expect("h2 handshake should succeed on duplex");

    tokio::spawn(async move {
        let _ = conn.await;
    });

    (PooledConnection::new_h2(sender), shutdown_tx)
}

#[tokio::test]
async fn checkout_coalesced_removes_not_ready_connection() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let (mut conn, shutdown_tx) = make_h2_conn_with_shutdown().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    // Shut down the server side to make the connection not-ready
    let _ = shutdown_tx.send(());
    // Give the connection driver time to detect the close
    tokio::time::sleep(Duration::from_millis(50)).await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    // Connection should be found by SAN but be not-ready, triggering the remove path
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    // The not-ready connection gets removed from the pool, None is returned
    assert!(
        result.is_none(),
        "not-ready connection should be removed and return None"
    );
}

#[tokio::test]
async fn checkout_coalesced_cleans_san_index_when_no_connections_remain() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let (mut conn, shutdown_tx) = make_h2_conn_with_shutdown().await;
    conn.sans = std::sync::Arc::from(vec!["origin.example.com".into(), "cdn.example.com".into()]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k.clone(), conn);

    // Shut down the server side
    let _ = shutdown_tx.send(());
    tokio::time::sleep(Duration::from_millis(50)).await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    // First checkout_coalesced: removes not-ready connection, cleans up
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(result.is_none());

    // Second lookup should also be None (SAN index was cleaned)
    let result2 = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(result2.is_none());

    // The pool key queue should be empty now
    assert!(
        pool.checkout(&k).is_none(),
        "pool should have no connections left"
    );
}

#[tokio::test]
async fn checkout_coalesced_san_index_stale_entry_continue_on_missing_queue() {
    // Test the path where san_index has a key but idle map doesn't (line 231: continue).
    // This happens when two different keys map to the same SAN: the first key's
    // idle queue might be empty/removed, so checkout_coalesced continues to the next key.
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k1 = key_https("origin1.example.com:443");
    let k2 = key_https("origin2.example.com:443");

    // Add a connection under k1 that shares SANs with k2, then make it not-ready
    let (mut conn1, shutdown1) = make_h2_conn_with_shutdown().await;
    conn1.sans = std::sync::Arc::from(vec!["origin1.example.com".into(), "cdn.example.com".into()]);
    conn1.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k1.clone(), conn1);

    // Add a healthy connection under k2 that also has the same SAN
    let mut conn2 = make_h2_conn().await;
    conn2.sans = std::sync::Arc::from(vec!["origin2.example.com".into(), "cdn.example.com".into()]);
    conn2.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k2.clone(), conn2);

    // Shut down k1's connection to make it not-ready
    let _ = shutdown1.send(());
    tokio::time::sleep(Duration::from_millis(50)).await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    // checkout_coalesced should skip k1 (not-ready) and find k2
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip));
    assert!(
        result.is_some(),
        "should find the second healthy connection even if first is not-ready"
    );
}

// --- pool with zero max_idle_per_host ---

#[tokio::test]
async fn pool_zero_max_idle_always_evicts() {
    // Edge case: max_idle_per_host = 0 means nothing stays pooled
    // (In practice the pool stores at least 1 because pop_front happens before push_back)
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(1)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    // Should be able to checkout the one connection
    assert!(pool.checkout(&k).is_some());
    assert!(pool.checkout(&k).is_none());
}

#[tokio::test]
async fn max_idle_per_host_zero_drops_on_checkin() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(0)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    assert!(
        pool.checkout(&k).is_none(),
        "connection should be dropped when max_idle_per_host is 0"
    );
}

// --- checkout_coalesced: SAN in index but connection SANs don't match target ---
// Covers line 247: `!entry.connection.sans.iter().any(|s| s == target_host)`

#[tokio::test]
async fn checkout_coalesced_san_index_has_entry_but_conn_sans_dont_match() {
    // This tests the scenario where the SAN index is populated via a shared SAN,
    // but the actual connection's SANs don't include the specific target_host
    // we're looking for from a different lookup path.
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    // Connection has SANs: origin.example.com, shared.example.com
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "shared.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    // Now manually insert a stale entry in the SAN index for a host
    // that isn't actually in the connection's SANs.
    // We simulate this by looking up "shared.example.com" (which IS in SANs) -
    // that should succeed:
    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("shared.example.com", Some(ip));
    assert!(result.is_some(), "shared SAN should match");

    // And looking up a host NOT in the SANs should fail:
    let result = pool.checkout_coalesced("other.example.com", Some(ip));
    assert!(result.is_none(), "host not in SANs should not match");
}

// --- checkout_coalesced with H2 connection that has empty queue after removal ---
// Covers lines 265-266 (found_key path)

#[tokio::test]
async fn checkout_coalesced_last_connection_in_queue_triggers_removal() {
    // When the last connection in a queue is checkout out (non-multiplex path),
    // the queue becomes empty and found_key is set for removal.
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    // Add a single H1 connection with SANs (won't multiplex)
    let mut conn = make_h1_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "coalesced.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    // H1 connections are rejected by is_h2_or_h3 check (line 243),
    // so this returns None and pool remains intact
    let result = pool.checkout_coalesced("coalesced.example.com", Some(ip));
    assert!(result.is_none(), "H1 should be rejected by coalescing");

    // Regular checkout should still work
    let result = pool.checkout(&k);
    assert!(result.is_some(), "H1 connection should still be in pool");
}

// --- ensure_reaper_local test (covers lines 350-371) ---

#[tokio::test]
async fn ensure_reaper_local_removes_expired() {
    // Use TokioRuntime as RuntimeLocal (it implements RuntimeLocal via spawn_local)
    // Actually TokioRuntime implements RuntimePoll, not RuntimeLocal.
    // The local reaper is tested in tests_compio.rs. Let's just verify the
    // send-path reaper cleans the SAN index properly.
    let pool = ConnectionPool::<RequestBodySend>::new()
        .with_max_idle_per_host(1)
        .with_idle_timeout(Duration::from_millis(50));
    pool.ensure_reaper::<TokioRuntime>();
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "reaper-test.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k.clone(), conn);

    // Wait for idle timeout + reaper cycle to fire
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Both regular checkout and coalesced checkout should fail
    assert!(
        pool.checkout(&k).is_none(),
        "reaper should have removed expired connection"
    );
    let ip: IpAddr = [10, 0, 0, 1].into();
    assert!(
        pool.checkout_coalesced("reaper-test.example.com", Some(ip))
            .is_none(),
        "reaper should have cleaned SAN index"
    );
}

// --- checkin with SANs populates index for all SANs ---

#[tokio::test]
async fn checkin_populates_san_index_for_all_sans() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "san1.example.com".into(),
        "san2.example.com".into(),
        "san3.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k, conn);

    tokio::task::yield_now().await;

    let ip: IpAddr = [10, 0, 0, 1].into();
    // All SANs should be findable via coalescing
    assert!(
        pool.checkout_coalesced("san1.example.com", Some(ip))
            .is_some()
    );
    assert!(
        pool.checkout_coalesced("san2.example.com", Some(ip))
            .is_some()
    );
    assert!(
        pool.checkout_coalesced("san3.example.com", Some(ip))
            .is_some()
    );
}

// --- checkout with not-ready H1 connection falls through ---

#[tokio::test]
async fn checkout_skips_not_ready_h1_connection() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");

    let (mut conn, shutdown_tx) = make_h2_conn_with_shutdown().await;
    // Make it appear as H1 for this test by setting remote_addr
    conn.remote_addr = Some(std::net::SocketAddr::from(([1, 1, 1, 1], 80)));

    // Actually use an H1 connection for this
    let (client_io, server_io) = tokio::io::duplex(8192);
    // Drop server immediately to make connection not-ready
    drop(server_io);

    let io = TokioIo::new(client_io);
    let (sender, conn_task) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake");

    tokio::spawn(async move {
        let _ = conn_task.await;
    });

    let mut h1_conn = PooledConnection::new_h1(sender);
    h1_conn.remote_addr = Some(std::net::SocketAddr::from(([1, 1, 1, 1], 80)));
    pool.checkin(k.clone(), h1_conn);

    // Give the connection driver time to detect the closed server
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Checkout should skip the not-ready connection
    let result = pool.checkout(&k);
    assert!(
        result.is_none(),
        "not-ready H1 connection should be skipped"
    );

    // Clean up the unused shutdown_tx
    let _ = shutdown_tx.send(());
}

// --- checkout_coalesced: san_index key exists but idle map has no entry (line 230-231) ---

#[tokio::test]
async fn checkout_coalesced_san_index_key_but_idle_empty() {
    // Create a scenario where san_index maps a SAN to a key, but the idle map
    // no longer has that key (because we checked out the H1 connection via regular checkout).
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("origin.example.com:443");

    // Use H1 connection - regular checkout removes it completely from idle
    let mut conn = make_h1_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "origin.example.com".into(),
        "coalesce-target.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    // Regular checkout removes the H1 connection from the idle map entirely
    let out = pool.checkout(&k);
    assert!(out.is_some(), "regular checkout should succeed");

    // Now san_index still has "coalesce-target.example.com" -> k,
    // but idle map no longer has k. This hits the `None => continue` path (lines 230-231).
    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("coalesce-target.example.com", Some(ip));
    assert!(
        result.is_none(),
        "should return None when idle map has no entry for the san_index key"
    );
}

// --- checkout_coalesced: san_index cleanup when idle map doesn't have key (lines 284-288) ---

#[tokio::test]
async fn checkout_coalesced_san_index_cleanup_removes_empty_set() {
    // Test that after checkout_coalesced finds no idle queue for a key,
    // it cleans up the san_index entry (lines 283-288).
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key_https("only-origin.example.com:443");

    // Use H1 connection so regular checkout fully removes from idle
    let mut conn = make_h1_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "only-origin.example.com".into(),
        "cleanup-target.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k.clone(), conn);

    tokio::task::yield_now().await;

    // Regular checkout removes from idle (H1, not multiplexed)
    let _ = pool.checkout(&k);

    // Now checkout_coalesced should find san_index entry but no idle entry,
    // triggering the cleanup that removes the key from san_index and then
    // removes the empty set.
    let ip: IpAddr = [10, 0, 0, 1].into();
    let result = pool.checkout_coalesced("cleanup-target.example.com", Some(ip));
    assert!(result.is_none());

    // Verify the SAN index was cleaned: a second call should also be None
    // (the san_index entry should have been removed entirely)
    let result2 = pool.checkout_coalesced("cleanup-target.example.com", Some(ip));
    assert!(
        result2.is_none(),
        "SAN index should have been fully cleaned"
    );
}

// --- spawn_reaper exercises the reaper loop body (lines 312-336) ---

#[tokio::test]
async fn reaper_loop_runs_multiple_cycles() {
    // Verify the reaper can run multiple cycles and correctly clean up connections
    // added at different times.
    let pool = ConnectionPool::<RequestBodySend>::new()
        .with_max_idle_per_host(4)
        .with_idle_timeout(Duration::from_millis(30));
    pool.ensure_reaper::<TokioRuntime>();
    let k = key("reaper-multi.example.com:80");

    // Add first connection
    let mut conn1 = make_h1_conn().await;
    conn1.remote_addr = Some(std::net::SocketAddr::from(([1, 1, 1, 1], 80)));
    pool.checkin(k.clone(), conn1);

    // Wait for first reaper cycle to expire it
    tokio::time::sleep(Duration::from_millis(80)).await;

    // First connection should be gone
    assert!(pool.checkout(&k).is_none(), "first conn should be reaped");

    // Add second connection
    let mut conn2 = make_h1_conn().await;
    conn2.remote_addr = Some(std::net::SocketAddr::from(([2, 2, 2, 2], 80)));
    pool.checkin(k.clone(), conn2);

    // Wait for second reaper cycle
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Second connection should also be gone
    assert!(pool.checkout(&k).is_none(), "second conn should be reaped");
}

// --- reaper cleans san_index entries with keys no longer in idle (lines 330-334) ---

#[tokio::test]
async fn reaper_cleans_san_index_keys_not_in_idle() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .with_max_idle_per_host(4)
        .with_idle_timeout(Duration::from_millis(30));
    pool.ensure_reaper::<TokioRuntime>();
    let k = key_https("reaper-san.example.com:443");

    let mut conn = make_h2_conn().await;
    conn.sans = std::sync::Arc::from(vec![
        "reaper-san.example.com".into(),
        "reaper-alt.example.com".into(),
    ]);
    conn.remote_addr = Some(std::net::SocketAddr::from(([10, 0, 0, 1], 443)));
    pool.checkin(k.clone(), conn);

    // Wait for connection to expire and reaper to clean up
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Both regular and coalesced lookups should fail
    assert!(pool.checkout(&k).is_none());
    let ip: IpAddr = [10, 0, 0, 1].into();
    assert!(
        pool.checkout_coalesced("reaper-alt.example.com", Some(ip))
            .is_none()
    );
}

// --- Multiple connections: checkout prefers the most-recently-added ready one ---

#[tokio::test]
async fn checkout_tries_multiple_candidates_lifo() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");

    // Add two connections
    let mut conn1 = make_h1_conn().await;
    conn1.remote_addr = Some(std::net::SocketAddr::from(([1, 1, 1, 1], 80)));
    pool.checkin(k.clone(), conn1);

    let mut conn2 = make_h1_conn().await;
    conn2.remote_addr = Some(std::net::SocketAddr::from(([2, 2, 2, 2], 80)));
    pool.checkin(k.clone(), conn2);

    tokio::task::yield_now().await;

    // LIFO: should get conn2 first (most recently added)
    let out1 = pool.checkout(&k).expect("first checkout");
    assert_eq!(
        out1.remote_addr,
        Some(std::net::SocketAddr::from(([2, 2, 2, 2], 80)))
    );

    // Then conn1
    let out2 = pool.checkout(&k).expect("second checkout");
    assert_eq!(
        out2.remote_addr,
        Some(std::net::SocketAddr::from(([1, 1, 1, 1], 80)))
    );

    // Then empty
    assert!(pool.checkout(&k).is_none());
}

#[tokio::test]
async fn evict_removes_all_connections_for_key() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");

    let conn1 = make_h1_conn().await;
    pool.checkin(k.clone(), conn1);
    let conn2 = make_h1_conn().await;
    pool.checkin(k.clone(), conn2);

    tokio::task::yield_now().await;

    pool.evict(&k);
    assert!(
        pool.checkout(&k).is_none(),
        "evict should remove all connections for the key"
    );
}

#[tokio::test]
async fn evict_does_not_affect_other_keys() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k1 = key("a.example.com:80");
    let k2 = key("b.example.com:80");

    let conn1 = make_h1_conn().await;
    pool.checkin(k1.clone(), conn1);
    let conn2 = make_h1_conn().await;
    pool.checkin(k2.clone(), conn2);

    tokio::task::yield_now().await;

    pool.evict(&k1);
    assert!(pool.checkout(&k1).is_none());
    assert!(
        pool.checkout(&k2).is_some(),
        "evict should not affect other keys"
    );
}

// --- max_active_per_host tests ---

#[tokio::test]
async fn max_active_per_host_blocks_when_at_cap() {
    let max_active = std::num::NonZeroUsize::new(1).unwrap();
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_per_host(Some(max_active));
    let k = key("example.com:80");

    assert!(
        pool.can_connect(&k),
        "can_connect should return true initially"
    );

    // Checkin a connection, then checkout — active count becomes 1
    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);
    tokio::task::yield_now().await;

    let _out = pool.checkout(&k).expect("first checkout should succeed");
    assert!(
        !pool.can_connect(&k),
        "can_connect should return false when at cap"
    );
}

#[tokio::test]
async fn checkin_frees_active_slot() {
    let max_active = std::num::NonZeroUsize::new(1).unwrap();
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_per_host(Some(max_active));
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);
    tokio::task::yield_now().await;

    let out = pool.checkout(&k).expect("first checkout");
    assert!(!pool.can_connect(&k), "at cap after checkout");

    // Return the connection — active slot is freed
    pool.checkin(k.clone(), out);
    assert!(
        pool.can_connect(&k),
        "can_connect should return true after checkin frees the slot"
    );

    // Should be able to checkout again
    let out2 = pool.checkout(&k);
    assert!(out2.is_some(), "should checkout after checkin freed slot");
}

#[tokio::test]
async fn drop_frees_active_slot() {
    let max_active = std::num::NonZeroUsize::new(1).unwrap();
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_per_host(Some(max_active));
    let k = key("example.com:80");

    let conn = make_h1_conn().await;
    pool.checkin(k.clone(), conn);
    tokio::task::yield_now().await;

    let out = pool.checkout(&k).expect("first checkout");
    assert!(!pool.can_connect(&k), "at cap after checkout");

    // Drop without checkin — the Drop impl decrements active count
    drop(out);
    assert!(
        pool.can_connect(&k),
        "can_connect should return true after drop frees the slot"
    );
}

#[test]
fn max_active_per_host_none_means_unlimited() {
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30));
    let k = key("example.com:80");

    // Default is unlimited — can always connect
    assert!(pool.can_connect(&k));
}

#[test]
fn max_active_per_host_zero_disables_cap() {
    // Passing 0 to the builder setter results in None (via NonZeroUsize::new(0))
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_per_host(None);
    let k = key("example.com:80");
    assert!(pool.can_connect(&k));
}

#[tokio::test]
async fn per_host_isolation() {
    let max_active = std::num::NonZeroUsize::new(1).unwrap();
    let pool = ConnectionPool::<RequestBodySend>::new()
        .without_reaper()
        .with_max_idle_per_host(8)
        .with_idle_timeout(Duration::from_secs(30))
        .with_max_active_per_host(Some(max_active));
    let k1 = key("a.example.com:80");
    let k2 = key("b.example.com:80");

    assert!(pool.can_connect(&k1));
    assert!(pool.can_connect(&k2));

    // Checkout for k1 puts it at cap
    let conn1 = make_h1_conn().await;
    pool.checkin(k1.clone(), conn1);
    tokio::task::yield_now().await;
    let _out1 = pool.checkout(&k1).expect("checkout k1");

    // k1 is at cap, k2 is not affected
    assert!(!pool.can_connect(&k1), "k1 should be at cap");
    assert!(
        pool.can_connect(&k2),
        "k2 should not be affected by k1's cap"
    );
}
