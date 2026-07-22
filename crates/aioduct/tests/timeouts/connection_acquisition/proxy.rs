use super::*;
use crate::RecordingObserver;

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_tls_handshake_uses_the_connection_deadline() {
    let (proxy_addr, task) = start_stalled_tcp_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .proxy(ProxyConfig::https(&format!("https://{proxy_addr}")).unwrap())
        .connect_timeout(Duration::from_millis(80))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get("http://origin.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    task.abort();
    assert_connect_timeout(&error);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn completed_https_proxy_phases_survive_connection_deadline_cancellation() {
    let (proxy_addr, proxy_cert, task) = start_tls_connect_then_stall().await;
    let observer = RecordingObserver::default();
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&proxy_cert),
    );
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port())).unwrap())
        .request_observer(observer.clone())
        .connect_timeout(Duration::from_millis(300))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let error = client
        .get("http://origin.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    task.abort();
    assert_connect_timeout(&error);
    let phases = observer.phases();
    assert!(
        phases.iter().any(|phase| phase == "TcpConnected"),
        "completed proxy TCP phase was lost on cancellation: {phases:?}"
    );
    assert!(
        phases.iter().any(|phase| phase == "TlsHandshakeComplete"),
        "completed proxy TLS phase was lost on cancellation: {phases:?}"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn proxied_origin_tls_handshake_uses_the_connection_deadline() {
    let (proxy_addr, task) = start_connect_then_stall().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .proxy(ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .connect_timeout(Duration::from_millis(80))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get("https://origin.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    task.abort();
    assert_connect_timeout(&error);
}

#[tokio::test]
async fn connect_response_uses_the_connection_deadline() {
    let (proxy_addr, task) = start_stalled_tcp_server().await;
    let observer = RecordingObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(observer.clone())
        .connect_timeout(Duration::from_millis(80))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get("http://origin.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    task.abort();
    assert_connect_timeout(&error);
    let phases = observer.phases();
    assert!(
        phases.iter().any(|phase| phase == "TcpConnected"),
        "completed proxy TCP phase was lost on cancellation: {phases:?}"
    );
}

#[tokio::test]
async fn socks4_negotiation_uses_the_connection_deadline() {
    let (proxy_addr, task) = start_stalled_tcp_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(ProxyConfig::socks4(&format!("socks4a://{proxy_addr}")).unwrap())
        .connect_timeout(Duration::from_millis(80))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get("http://origin.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    task.abort();
    assert_connect_timeout(&error);
}

#[tokio::test]
async fn socks5_negotiation_uses_the_connection_deadline() {
    let (proxy_addr, task) = start_stalled_tcp_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(ProxyConfig::socks5h(&format!("socks5h://{proxy_addr}")).unwrap())
        .connect_timeout(Duration::from_millis(80))
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    let error = client
        .get("http://origin.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    task.abort();
    assert_connect_timeout(&error);
}

#[tokio::test]
async fn chained_proxy_hops_share_one_connection_budget() {
    let delay = Duration::from_millis(200);
    let (first_addr, second_addr, first_task, second_task) =
        start_delayed_two_hop_chain(delay).await;
    let chain = ProxyChain::new(vec![
        ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
        ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
    ]);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(chain)
        .connect_timeout(Duration::from_millis(300))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let error = client
        .get("http://origin.test/")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    first_task.abort();
    second_task.abort();
    assert_connect_timeout(&error);
}
