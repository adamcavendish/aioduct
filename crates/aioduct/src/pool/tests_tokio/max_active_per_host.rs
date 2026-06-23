use super::*;

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
