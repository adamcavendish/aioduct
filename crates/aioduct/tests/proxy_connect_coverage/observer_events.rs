use super::*;

#[cfg(feature = "rustls")]
#[tokio::test]
async fn observer_fires_tls_timing_on_https() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server().await;

    let cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[cert])
        .danger_accept_invalid_hostnames(true)
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello tls");

    let phases = obs.phases();

    // Must contain TcpConnected (dispatch_send.rs:782-787)
    assert!(
        phases.contains(&"TcpConnected".to_string()),
        "HTTPS request should fire TcpConnected, got: {phases:?}"
    );

    // Must contain TlsHandshakeComplete (dispatch_send.rs:792-806)
    assert!(
        phases.contains(&"TlsHandshakeComplete".to_string()),
        "HTTPS request should fire TlsHandshakeComplete, got: {phases:?}"
    );

    // Verify TlsHandshakeComplete has the expected ALPN and duration data
    let tls_events: Vec<_> = obs
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|p| match p {
            RequestPhase::TlsHandshakeComplete {
                duration,
                alpn_protocol,
                peer_certificate_der,
            } => Some((
                *duration,
                alpn_protocol.clone(),
                peer_certificate_der.clone(),
            )),
            _ => None,
        })
        .collect();

    assert_eq!(
        tls_events.len(),
        1,
        "should have exactly one TlsHandshakeComplete event"
    );
    let (tls_dur, alpn, peer_cert) = &tls_events[0];
    assert!(
        *tls_dur > Duration::ZERO,
        "TLS handshake duration should be positive"
    );
    assert_eq!(
        alpn.as_deref(),
        Some("h2"),
        "ALPN should be h2 for H2 server"
    );
    assert!(
        peer_cert.is_some(),
        "peer certificate DER should be present for TLS connection"
    );

    // Verify TcpConnected has correct data
    let tcp_events: Vec<_> = obs
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|p| match p {
            RequestPhase::TcpConnected {
                remote_addr,
                duration,
                protocol,
            } => Some((*remote_addr, *duration, *protocol)),
            _ => None,
        })
        .collect();

    assert_eq!(
        tcp_events.len(),
        1,
        "should have exactly one TcpConnected event"
    );
    let (tcp_addr, tcp_dur, _proto) = &tcp_events[0];
    assert_eq!(tcp_addr.port(), addr.port());
    assert!(
        *tcp_dur >= Duration::ZERO,
        "TCP connect duration should be non-negative"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 2. Observer receives TlsHandshakeComplete with H1 ALPN
//    Exercises the H1 branch of dispatch_send.rs:794-796.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(feature = "rustls")]
#[tokio::test]
async fn observer_tls_h1_alpn_protocol() {
    aioduct_test_server::tls::install_crypto_provider();

    let (addr, cert_der, _counter) = aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;

    let cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[cert])
        .danger_accept_invalid_hostnames(true)
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();

    let tls_events: Vec<_> = obs
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|p| match p {
            RequestPhase::TlsHandshakeComplete { alpn_protocol, .. } => Some(alpn_protocol.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(tls_events.len(), 1);
    assert_eq!(
        tls_events[0].as_deref(),
        Some("http/1.1"),
        "ALPN should be http/1.1 for H1-only TLS server"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 3. Observer on plain HTTP shows TcpConnected but NO TlsHandshakeComplete
//    Exercises dispatch_send.rs:821-833 (non-TLS tcp_connect path).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn observer_plain_http_no_tls_event() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let obs = RecordingObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();

    let phases = obs.phases();
    assert!(
        phases.contains(&"TcpConnected".to_string()),
        "plain HTTP should fire TcpConnected, got: {phases:?}"
    );
    assert!(
        !phases.contains(&"TlsHandshakeComplete".to_string()),
        "plain HTTP should NOT fire TlsHandshakeComplete, got: {phases:?}"
    );
}
