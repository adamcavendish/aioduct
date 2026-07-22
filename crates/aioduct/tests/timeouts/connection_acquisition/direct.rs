#[cfg(feature = "rustls")]
use super::super::RecordingObserver;
use super::*;

#[tokio::test]
async fn dns_resolution_uses_the_connection_deadline() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .resolver(PendingResolver)
        .connect_timeout(Duration::from_millis(60))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get("http://pending.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert_connect_timeout(&error);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn coalescing_dns_uses_the_connection_deadline() {
    let observer = RecordingObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .resolver(PendingResolver)
        .request_observer(observer.clone())
        .connect_timeout(Duration::from_millis(60))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get("https://pending.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert_connect_timeout(&error);
    assert!(
        observer
            .phases()
            .iter()
            .any(|phase| phase == "PoolCheckoutComplete(Miss)"),
        "coalescing timeout must complete the pool checkout observer phase"
    );
}

#[tokio::test]
async fn tcp_connect_uses_the_connection_deadline() {
    let connector = PendingConnector::default();
    let client =
        HttpEngineSend::<TokioRuntime, PendingConnector>::builder_with_connector(connector.clone())
            .connect_timeout(Duration::from_millis(60))
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

    let error = client
        .get("http://127.0.0.1:9/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert_connect_timeout(&error);
    assert_eq!(connector.attempts(), 1);
}

#[tokio::test]
async fn dns_and_tcp_share_one_connection_budget() {
    let (server_addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let phase_delay = Duration::from_millis(200);
    let connector = DelayedConnector {
        inner: TcpConnector,
        delay: phase_delay,
    };
    let client =
        HttpEngineSend::<TokioRuntime, DelayedConnector>::builder_with_connector(connector)
            .resolver(DelayedResolver {
                addr: server_addr,
                delay: phase_delay,
            })
            .connect_timeout(Duration::from_millis(300))
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();

    let error = client
        .get(&format!("http://delayed.test:{}/", server_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert_connect_timeout(&error);
}

#[tokio::test]
async fn connection_deadline_ends_before_response_waiting() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|_request| async {
        tokio::time::sleep(Duration::from_millis(120)).await;
        Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(Bytes::from_static(
            b"ok",
        ))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .connect_timeout(Duration::from_millis(50))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "ok");
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn tls_handshake_uses_the_connection_deadline() {
    let (addr, task) = start_stalled_tcp_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .connect_timeout(Duration::from_millis(80))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get(&format!("https://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap_err();

    task.abort();
    assert_connect_timeout(&error);
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn h3_handshake_uses_the_connection_deadline() {
    aioduct_test_server::tls::install_crypto_provider();
    let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = blackhole.local_addr().unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .connect_timeout(Duration::from_millis(100))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get(&format!("https://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap_err();

    drop(blackhole);
    assert_connect_timeout(&error);
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn h3_force_addr_uses_the_connection_deadline_path() {
    let (addr, _cert, _counter) = aioduct_test_server::h3::h3_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();

    let response = client
        .get(&format!("https://forced.test:{}/", addr.port()))
        .unwrap()
        .force_addr(addr)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.version(), http::Version::HTTP_3);
}
