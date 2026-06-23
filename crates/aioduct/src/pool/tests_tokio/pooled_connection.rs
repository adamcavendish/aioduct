use super::{make_h1_conn, make_h2_conn};

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
    assert_eq!(cloned.requests_served(), 1);
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
