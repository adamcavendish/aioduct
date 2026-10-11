use super::*;

#[cfg(feature = "rustls")]
#[tokio::test]
async fn observer_fires_tls_timing_on_https() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert_der, _) = aioduct_test_server::tls::tls_h2_server().await;
    let obs = RecordingObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[aioduct::tls::Certificate::from_der(cert_der.to_vec())])
        .danger_accept_invalid_hostnames(true)
        .observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.version(), http::Version::HTTP_2);
    assert_eq!(resp.remote_addr().unwrap().port(), addr.port());
    assert!(resp.tls_info().unwrap().peer_certificate().is_some());
    assert_eq!(resp.text().await.unwrap(), "hello tls");
    let phases = obs.phases();
    assert_eq!(
        phases
            .iter()
            .filter(|p| *p == "EstablishmentComplete(TcpConnect)")
            .count(),
        1
    );
    assert_eq!(
        phases
            .iter()
            .filter(|p| *p == "EstablishmentComplete(TlsHandshake)")
            .count(),
        1
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn observer_tls_h1_protocol() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert_der, _) = aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let obs = RecordingObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[aioduct::tls::Certificate::from_der(cert_der.to_vec())])
        .danger_accept_invalid_hostnames(true)
        .observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(&format!("https://localhost:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.version(), http::Version::HTTP_11);
    let _ = resp.text().await.unwrap();
    assert_eq!(
        obs.phases()
            .iter()
            .filter(|p| *p == "EstablishmentComplete(TlsHandshake)")
            .count(),
        1
    );
}

#[tokio::test]
async fn observer_plain_http_no_tls_event() {
    let (addr, _) = aioduct_test_server::h1::h1_server().await;
    let obs = RecordingObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .observer(obs.clone())
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
        phases
            .iter()
            .any(|p| p == "EstablishmentComplete(TcpConnect)")
    );
    assert!(
        !phases
            .iter()
            .any(|p| p == "EstablishmentComplete(TlsHandshake)")
    );
}
