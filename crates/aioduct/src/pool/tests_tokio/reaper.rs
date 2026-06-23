use super::*;

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
    let result = pool.checkout_coalesced("cdn.example.com", Some(ip), ProxyRoute::DIRECT);
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
