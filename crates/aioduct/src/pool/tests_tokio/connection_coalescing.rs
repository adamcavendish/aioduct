use super::*;
use std::net::IpAddr;

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
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip), ProxyRoute::DIRECT);
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
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip), ProxyRoute::DIRECT);
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
    let result = pool.checkout_coalesced("cdn.example.com", Some(different_ip), ProxyRoute::DIRECT);
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
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip), ProxyRoute::DIRECT);
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
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip), ProxyRoute::DIRECT);
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
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip), ProxyRoute::DIRECT);
    assert!(
        result.is_none(),
        "connection past max lifetime should not be returned"
    );
}
