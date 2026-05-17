use super::*;
use crate::body::RequestBodySend;
use crate::runtime::SmolRuntime;
use crate::runtime::smol_rt::SmolIo;
use crate::runtime::{RuntimeCompletion, RuntimePoll};

async fn make_h1_conn() -> PooledConnection<RequestBodySend> {
    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let client_tcp = smol::net::TcpStream::connect(addr).await.unwrap();
    let (server_tcp, _) = listener.accept().await.unwrap();

    smol::spawn(async move {
        use smol::io::AsyncReadExt;
        let mut server_tcp = server_tcp;
        let mut buf = [0u8; 1024];
        loop {
            match server_tcp.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
        }
    })
    .detach();

    let io = SmolIo::new(client_tcp);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake should succeed");

    SmolRuntime::spawn_send(async move {
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
    let pool = ConnectionPool::<RequestBodySend>::new_no_reaper(8, Duration::from_secs(30));
    assert!(pool.checkout(&key("example.com:80")).is_none());
}

#[test]
fn checkin_then_checkout_returns_connection() {
    smol::block_on(async {
        let pool = ConnectionPool::<RequestBodySend>::new_no_reaper(8, Duration::from_secs(30));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        SmolRuntime::sleep(Duration::from_millis(10)).await;

        let out = pool.checkout(&k);
        assert!(
            out.is_some(),
            "checkout should return the checked-in connection"
        );
    });
}

#[test]
fn checkout_with_different_key_returns_none() {
    smol::block_on(async {
        let pool = ConnectionPool::<RequestBodySend>::new_no_reaper(8, Duration::from_secs(30));

        let conn = make_h1_conn().await;
        pool.checkin(key("a.example.com:80"), conn);

        SmolRuntime::sleep(Duration::from_millis(10)).await;

        assert!(
            pool.checkout(&key("b.example.com:80")).is_none(),
            "checkout with a different key should return None"
        );
    });
}

#[test]
fn pool_respects_max_idle_per_host() {
    smol::block_on(async {
        let max_idle = 2;
        let pool =
            ConnectionPool::<RequestBodySend>::new_no_reaper(max_idle, Duration::from_secs(30));
        let k = key("example.com:80");

        for _ in 0..3 {
            let conn = make_h1_conn().await;
            pool.checkin(k.clone(), conn);
        }

        SmolRuntime::sleep(Duration::from_millis(10)).await;

        assert!(pool.checkout(&k).is_some(), "1st checkout should succeed");
        assert!(pool.checkout(&k).is_some(), "2nd checkout should succeed");
        assert!(
            pool.checkout(&k).is_none(),
            "3rd checkout should return None (capacity was 2)"
        );
    });
}

#[test]
fn checkin_checkout_is_lifo() {
    smol::block_on(async {
        let pool = ConnectionPool::<RequestBodySend>::new_no_reaper(8, Duration::from_secs(30));
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

        SmolRuntime::sleep(Duration::from_millis(10)).await;

        let out = pool.checkout(&k).expect("should get a connection");
        assert_eq!(
            out.remote_addr,
            Some(addr2),
            "LIFO: most recent connection first"
        );
    });
}

#[test]
fn checkout_expired_connection_returns_none() {
    smol::block_on(async {
        let pool = ConnectionPool::<RequestBodySend>::new_no_reaper(8, Duration::from_millis(50));
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        SmolRuntime::sleep(Duration::from_millis(100)).await;

        assert!(
            pool.checkout(&k).is_none(),
            "expired connection should be discarded"
        );
    });
}

#[test]
fn reaper_removes_expired_connections() {
    smol::block_on(async {
        let pool = ConnectionPool::<RequestBodySend>::new(1, Duration::from_millis(50));
        pool.ensure_reaper::<SmolRuntime>();
        let k = key("example.com:80");

        let conn = make_h1_conn().await;
        pool.checkin(k.clone(), conn);

        SmolRuntime::sleep(Duration::from_millis(150)).await;

        assert!(
            pool.checkout(&k).is_none(),
            "reaper should have removed the expired connection"
        );
    });
}
