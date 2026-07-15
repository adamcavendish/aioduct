#![cfg(feature = "tokio")]

#[path = "proxy/common.rs"]
mod common;
#[cfg(feature = "rustls")]
#[path = "proxy/incoming_multipart.rs"]
mod incoming_multipart;
#[cfg(feature = "rustls")]
#[path = "proxy/mixed_chain.rs"]
mod mixed_chain;
#[path = "proxy/no_proxy.rs"]
mod no_proxy;
#[path = "proxy/socks.rs"]
mod socks;

use common::*;

#[cfg(feature = "rustls-aws-lc-rs")]
#[derive(Clone, Default)]
struct EchPreflightSendConnector {
    attempts: Arc<AtomicUsize>,
}

#[cfg(feature = "rustls-aws-lc-rs")]
impl ConnectorSend for EchPreflightSendConnector {
    type Stream = <TcpConnector as ConnectorSend>::Stream;

    fn connect(&self, _addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let attempts = Arc::clone(&self.attempts);
        async move {
            attempts.fetch_add(1, AtomicOrdering::SeqCst);
            Err(io::Error::other("ECH preflight reached the connector"))
        }
    }
}

#[cfg(feature = "rustls-aws-lc-rs")]
fn ech_grease_connector() -> aioduct::tls::RustlsConnector {
    use rustls::crypto::hpke::Hpke as _;

    let hpke = rustls::crypto::aws_lc_rs::hpke::DH_KEM_P256_HKDF_SHA256_AES_128;
    let (placeholder_key, _) = hpke.generate_key_pair().expect("HPKE key pair");
    let ech_mode = rustls::client::EchMode::Grease(rustls::client::EchGreaseConfig::new(
        hpke,
        placeholder_key,
    ));
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_ech(ech_mode)
    .expect("ECH config")
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    aioduct::tls::RustlsConnector::new(Arc::new(config))
}

#[derive(Clone, Default)]
struct ProxyPhaseObserver(Arc<std::sync::Mutex<Vec<&'static str>>>);

impl aioduct::observer::RequestObserver for ProxyPhaseObserver {
    fn on_event(&self, event: &aioduct::observer::RequestEvent) {
        let phase = match &event.phase {
            aioduct::observer::RequestPhase::DnsResolved { .. } => Some("dns"),
            aioduct::observer::RequestPhase::TcpConnected { .. } => Some("tcp"),
            aioduct::observer::RequestPhase::TlsHandshakeComplete { .. } => Some("tls"),
            _ => None,
        };
        if let Some(phase) = phase {
            self.0.lock().unwrap().push(phase);
        }
    }

    fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
}

impl ProxyPhaseObserver {
    fn phases(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

#[cfg(feature = "rustls")]
#[derive(Clone, Default)]
struct TlsAlpnObserver(Arc<std::sync::Mutex<Vec<Option<String>>>>);

#[cfg(feature = "rustls")]
impl aioduct::observer::RequestObserver for TlsAlpnObserver {
    fn on_event(&self, event: &aioduct::observer::RequestEvent) {
        if let aioduct::observer::RequestPhase::TlsHandshakeComplete { alpn_protocol, .. } =
            &event.phase
        {
            self.0.lock().unwrap().push(alpn_protocol.clone());
        }
    }

    fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
}

#[cfg(feature = "rustls")]
impl TlsAlpnObserver {
    fn negotiated_protocols(&self) -> Vec<Option<String>> {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LiveProxyPhase {
    Tcp(aioduct::NegotiatedProtocol),
    Tls(Option<String>),
}

#[derive(Clone)]
struct LiveProxyPhaseObserver(tokio::sync::mpsc::UnboundedSender<LiveProxyPhase>);

impl aioduct::observer::RequestObserver for LiveProxyPhaseObserver {
    fn on_event(&self, event: &aioduct::observer::RequestEvent) {
        let phase = match &event.phase {
            aioduct::observer::RequestPhase::TcpConnected { protocol, .. } => {
                Some(LiveProxyPhase::Tcp(*protocol))
            }
            aioduct::observer::RequestPhase::TlsHandshakeComplete { alpn_protocol, .. } => {
                Some(LiveProxyPhase::Tls(alpn_protocol.clone()))
            }
            _ => None,
        };
        if let Some(phase) = phase {
            let _ = self.0.send(phase);
        }
    }

    fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
}

async fn silent_h2c_then_h1_origin() -> SocketAddr {
    use tokio::io::AsyncReadExt as _;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let connection = accepted.fetch_add(1, AtomicOrdering::SeqCst);
            tokio::spawn(async move {
                if connection == 0 {
                    let mut preface = [0_u8; 24];
                    stream.read_exact(&mut preface).await.unwrap();
                    assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
                    std::future::pending::<()>().await;
                }

                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        hyper::service::service_fn(|_| async {
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                "h1 fallback",
                            ))))
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn proxy_tcp_observer_phase_fires_while_connect_is_stalled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let server_release = release.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        server_release.notified().await;
        drop(stream);
    });

    let (phase_tx, mut phase_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(LiveProxyPhaseObserver(phase_tx))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let request = tokio::spawn(async move {
        client
            .get("http://origin.test/observer-stall")
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
    });

    let phase = tokio::time::timeout(std::time::Duration::from_secs(1), phase_rx.recv())
        .await
        .expect("proxy TCP observer event was buffered behind CONNECT")
        .unwrap();
    assert_eq!(
        phase,
        LiveProxyPhase::Tcp(aioduct::NegotiatedProtocol::Http1)
    );
    assert!(
        !request.is_finished(),
        "request completed before CONNECT resumed"
    );

    release.notify_one();
    assert!(request.await.unwrap().is_err());
}

#[tokio::test]
async fn invalid_proxy_plans_fail_before_opening_a_proxy_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();

    let invalid_header = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .header(
            http::header::HeaderName::from_static("x-binary"),
            http::HeaderValue::from_bytes(&[0x80]).unwrap(),
        );
    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(invalid_header)
        .build()
        .unwrap()
        .get("http://origin.test/header")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("CONNECT headers"));

    let framing_header = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .header(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_static("1"),
        );
    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(framing_header)
        .build()
        .unwrap()
        .get("http://origin.test/framing-header")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("controlled by aioduct"));

    let conflicting_auth = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .basic_auth("user", "password")
        .header(
            http::header::PROXY_AUTHORIZATION,
            http::HeaderValue::from_static("Bearer token"),
        );
    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(conflicting_auth)
        .build()
        .unwrap()
        .get("http://origin.test/conflicting-auth")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("one Proxy-Authorization source"));

    let socks4a = aioduct::ProxyConfig::socks4(&format!("socks4a://{proxy_addr}")).unwrap();
    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(socks4a)
        .build()
        .unwrap()
        .get("http://[::1]/ipv6")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("SOCKS4"));
    assert!(error.to_string().contains("IPv6"));

    let nul_user =
        aioduct::ProxyConfig::socks4(&format!("socks4://user%00name@{proxy_addr}")).unwrap();
    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(nul_user)
        .build()
        .unwrap()
        .get("http://origin.test/nul-user")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("NUL"));

    let long_user = "u".repeat(256);
    let long_credentials = aioduct::ProxyConfig::socks5(&format!("socks5://{proxy_addr}"))
        .unwrap()
        .basic_auth(&long_user, "password");
    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(long_credentials)
        .build()
        .unwrap()
        .get("http://origin.test/long-credentials")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("255 bytes"));

    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "invalid proxy configuration reached the network"
    );
}

#[cfg(feature = "rustls-aws-lc-rs")]
#[tokio::test]
async fn ech_https_proxy_hops_fail_before_dns_or_connector_io() {
    for https_hop in 0..2 {
        let resolver_attempts = Arc::new(AtomicUsize::new(0));
        let resolver_counter = Arc::clone(&resolver_attempts);
        let connector = EchPreflightSendConnector::default();
        let proxies = if https_hop == 0 {
            vec![
                aioduct::ProxyConfig::https("https://first-proxy.test:8443").unwrap(),
                aioduct::ProxyConfig::http("http://second-proxy.test:8080").unwrap(),
            ]
        } else {
            vec![
                aioduct::ProxyConfig::http("http://first-proxy.test:8080").unwrap(),
                aioduct::ProxyConfig::https("https://second-proxy.test:8443").unwrap(),
            ]
        };
        let client =
            HttpEngineSend::<TokioRuntime, EchPreflightSendConnector>::builder_with_connector(
                connector.clone(),
            )
            .tls(ech_grease_connector())
            .proxy_chain(aioduct::ProxyChain::new(proxies))
            .resolver(move |_host: &str, _port: u16| {
                resolver_counter.fetch_add(1, AtomicOrdering::SeqCst);
                Box::pin(async { Ok("127.0.0.1:9".parse().unwrap()) })
                    as std::pin::Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>>
            })
            .build()
            .unwrap();

        let error = client
            .get("http://origin.test/ech-preflight")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("cannot inherit an ECH-enabled origin configuration"),
            "unexpected ECH preflight error for hop {https_hop}: {error}"
        );
        assert_eq!(resolver_attempts.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(connector.attempts.load(AtomicOrdering::SeqCst), 0);
    }
}

#[tokio::test]
async fn overlong_socks5h_targets_fail_before_one_or_two_hop_proxy_io() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_addr = listener.local_addr().unwrap();
    let long_host = format!("{}.test", "a".repeat(256));

    let one_hop = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::socks5h(&format!("socks5h://{first_addr}")).unwrap())
        .build()
        .unwrap();
    let error = one_hop
        .get(&format!("http://{long_host}/one-hop"))
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("255 bytes"), "{error}");

    let remote_second = aioduct::ProxyConfig::http(&format!("http://{long_host}:8080")).unwrap();
    let two_hop = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(aioduct::ProxyChain::new(vec![
            aioduct::ProxyConfig::socks5h(&format!("socks5h://{first_addr}")).unwrap(),
            remote_second,
        ]))
        .build()
        .unwrap();
    let error = two_hop
        .get("http://origin.test/two-hop")
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("255 bytes"), "{error}");

    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "unencodable SOCKS5h target reached the first proxy"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_tcp_observer_fires_before_proxy_tls_stalls() {
    aioduct_test_server::tls::install_crypto_provider();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let server_release = release.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        server_release.notified().await;
        drop(stream);
    });

    let (phase_tx, mut phase_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://127.0.0.1:{}", proxy_addr.port()))
                .unwrap(),
        )
        .request_observer(LiveProxyPhaseObserver(phase_tx))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let request = tokio::spawn(async move {
        client
            .get("http://origin.test/proxy-tls-stall")
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
    });

    let phase = tokio::time::timeout(std::time::Duration::from_secs(1), phase_rx.recv())
        .await
        .expect("proxy TCP observer event was buffered behind proxy TLS")
        .unwrap();
    assert_eq!(
        phase,
        LiveProxyPhase::Tcp(aioduct::NegotiatedProtocol::Http1)
    );
    assert!(
        !request.is_finished(),
        "request completed before proxy TLS resumed"
    );

    release.notify_one();
    assert!(request.await.unwrap().is_err());
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn proxy_tls_observer_phase_fires_while_connect_is_stalled() {
    aioduct_test_server::tls::install_crypto_provider();
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let listener = TcpListener::bind("localhost:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let server_release = release.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(stream).await.unwrap();
        server_release.notified().await;
        drop(tls);
    });

    let (phase_tx, mut phase_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[aioduct::tls::Certificate::from_der(cert_der.to_vec())])
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .request_observer(LiveProxyPhaseObserver(phase_tx))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let request = tokio::spawn(async move {
        client
            .get("http://origin.test/proxy-tls-observer-stall")
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
    });

    let proxy_phases = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let mut phases = Vec::new();
        loop {
            let phase = phase_rx.recv().await.unwrap();
            let done = matches!(phase, LiveProxyPhase::Tls(_));
            phases.push(phase);
            if done {
                break phases;
            }
        }
    })
    .await
    .expect("proxy TLS observer event was buffered behind CONNECT");
    assert_eq!(
        proxy_phases,
        [
            LiveProxyPhase::Tcp(aioduct::NegotiatedProtocol::Http1),
            LiveProxyPhase::Tls(Some("http/1.1".to_owned())),
        ]
    );
    assert!(
        !request.is_finished(),
        "request completed before CONNECT resumed"
    );

    release.notify_one();
    assert!(request.await.unwrap().is_err());
}

#[tokio::test]
async fn test_http_proxy() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn force_addr_overrides_http_proxy_tunnel_destination() {
    let (target_addr, _counter) = h1_server().await;
    let captured_connects = captured_connects();
    let (proxy_addr, _connections) =
        connect_proxy_with_capture(Some(captured_connects.clone())).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();
    let response = client
        .get("http://unresolvable-force-addr.invalid:1/forced")
        .unwrap()
        .force_addr(target_addr)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    let requests = captured_connects.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(connect_target(&requests[0]), target_addr.to_string());
}

#[tokio::test]
async fn proxy_establishment_reports_dns_and_tcp_observer_phases() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;
    let observer = ProxyPhaseObserver::default();

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http(&format!(
                "http://observer-proxy.test:{}",
                proxy_addr.port()
            ))
            .unwrap(),
        )
        .resolve("observer-proxy.test", proxy_addr)
        .request_observer(observer.clone())
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/observer"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "hello aioduct");

    let phases = observer.phases();
    assert!(
        phases.contains(&"dns"),
        "missing proxy DNS phase: {phases:?}"
    );
    assert!(
        phases.contains(&"tcp"),
        "missing proxy TCP phase: {phases:?}"
    );
    assert!(
        !phases.contains(&"tls"),
        "plain proxy path emitted TLS: {phases:?}"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn proxy_establishment_reports_origin_tls_observer_phase() {
    aioduct_test_server::tls::install_crypto_provider();
    let (target_addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let (proxy_addr, _conns) = connect_proxy().await;
    let observer = ProxyPhaseObserver::default();
    let certificate = aioduct::tls::Certificate::from_der(cert_der.to_vec());

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[certificate])
        .danger_accept_invalid_hostnames(true)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(observer.clone())
        .build()
        .unwrap();

    let response = client
        .get(&format!(
            "https://localhost:{}/observer",
            target_addr.port()
        ))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "hello tls");

    let phases = observer.phases();
    assert!(
        phases.contains(&"dns"),
        "missing proxy DNS phase: {phases:?}"
    );
    assert!(
        phases.contains(&"tcp"),
        "missing proxy TCP phase: {phases:?}"
    );
    assert!(
        phases.contains(&"tls"),
        "missing origin TLS phase: {phases:?}"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn proxied_origin_tls_observer_preserves_missing_alpn() {
    aioduct_test_server::tls::install_crypto_provider();
    let (target_addr, cert_der, _) = aioduct_test_server::tls::tls_h1_server(&[]).await;
    let (proxy_addr, _) = connect_proxy().await;
    let observer = TlsAlpnObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[aioduct::tls::Certificate::from_der(cert_der.to_vec())])
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(observer.clone())
        .build()
        .unwrap();

    let response = client
        .get(&format!(
            "https://localhost:{}/missing-alpn",
            target_addr.port()
        ))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "hello tls");
    assert_eq!(observer.negotiated_protocols(), [None]);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn proxied_origin_tls_observer_fires_before_http_handshake_failure() {
    aioduct_test_server::tls::install_crypto_provider();
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = origin.accept().await.unwrap();
        let tls = acceptor.accept(stream).await.unwrap();
        drop(tls);
    });

    let (proxy_addr, _) = connect_proxy().await;
    let observer = TlsAlpnObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[aioduct::tls::Certificate::from_der(cert_der.to_vec())])
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(observer.clone())
        .build()
        .unwrap();

    client
        .get(&format!(
            "https://localhost:{}/http-handshake-abort",
            origin_addr.port()
        ))
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert_eq!(
        observer.negotiated_protocols(),
        [Some("http/1.1".to_owned())],
        "origin TLS completion must be observable even when HTTP setup fails"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_establishment_reports_proxy_tls_observer_phase() {
    aioduct_test_server::tls::install_crypto_provider();
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, proxy_cert, _connections) = tls_connect_proxy().await;
    let observer = ProxyPhaseObserver::default();
    let certificate = aioduct::tls::Certificate::from_der(proxy_cert.to_vec());

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[certificate])
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .request_observer(observer.clone())
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/secure-proxy-observer"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "hello aioduct");

    let phases = observer.phases();
    assert!(
        phases.contains(&"dns"),
        "missing proxy DNS phase: {phases:?}"
    );
    assert!(
        phases.contains(&"tcp"),
        "missing proxy TCP phase: {phases:?}"
    );
    assert!(
        phases.contains(&"tls"),
        "missing proxy TLS phase: {phases:?}"
    );
}

#[tokio::test]
async fn proxy_endpoint_falls_back_to_second_resolved_address() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;
    let unavailable_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable_addr = unavailable_listener.local_addr().unwrap();
    drop(unavailable_listener);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http("http://proxy.test:8080").unwrap())
        .resolve_to_addrs("proxy.test", &[unavailable_addr, proxy_addr])
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/proxy-address-fallback"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "hello aioduct");
}

#[tokio::test]
async fn http_proxy_endpoint_falls_back_after_connect_transport_failure() {
    let (target_addr, _counter) = h1_server().await;
    let (closing_addr, closing_connections) = closing_proxy_endpoint().await;
    let (proxy_addr, proxy_connections) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http("http://proxy.test:8080").unwrap())
        .resolve_to_addrs("proxy.test", &[closing_addr, proxy_addr])
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/post-tcp-connect-fallback"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert_eq!(closing_connections.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(proxy_connections.load(AtomicOrdering::SeqCst), 1);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_endpoint_falls_back_after_tls_transport_failure() {
    let (target_addr, _counter) = h1_server().await;
    let (closing_addr, closing_connections) = closing_proxy_endpoint().await;
    let (proxy_addr, proxy_cert, proxy_connections) = tls_connect_proxy().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&proxy_cert),
    );

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(aioduct::ProxyConfig::https("https://localhost:8443").unwrap())
        .resolve_to_addrs("localhost", &[closing_addr, proxy_addr])
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/post-tcp-tls-fallback"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert_eq!(closing_connections.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(proxy_connections.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test]
async fn test_http_proxy_basic_auth() {
    let (target_addr, _counter) = h1_server().await;
    let captured_connects = captured_connects();
    let (proxy_addr, _conns) = connect_proxy_with_capture(Some(captured_connects.clone())).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/prox"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert_connect_for_target_has_auth(&captured_connects, &target_addr.to_string());
}

#[tokio::test]
async fn http_proxy_uri_auth_reaches_connect_tunnel() {
    let (target_addr, _counter) = h1_server().await;
    let captured_connects = captured_connects();
    let (proxy_addr, _conns) = connect_proxy_with_capture(Some(captured_connects.clone())).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://Aladdin:open%20sesame@{proxy_addr}"))
                .unwrap(),
        )
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/uri-auth"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert_connect_for_target_has_auth(&captured_connects, &target_addr.to_string());
}

#[tokio::test]
async fn test_http_proxy_preserves_host_header() {
    // With CONNECT tunnel, the target directly receives the Host header.
    let (target_addr, _counter) = h1_server_with(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let method = req.method().to_string();
        let body = format!("method={method} host={host}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;
    let (proxy_addr, _conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("host="), "expected host in body, got: {body}");
}

#[tokio::test]
async fn test_connect_tunnel_includes_proxy_auth() {
    // Simulate a proxy that receives a CONNECT request.
    // We parse the raw CONNECT to check for Proxy-Authorization.
    let auth_seen = Arc::new(AtomicBool::new(false));
    let auth_seen_clone = auth_seen.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let auth_seen = auth_seen_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);

            // Check that this is a CONNECT request
            if req_str.starts_with("CONNECT") {
                // Check for Proxy-Authorization header
                for line in req_str.lines() {
                    if line.to_lowercase().starts_with("proxy-authorization:") {
                        let value = line.split_once(':').map(|x| x.1).unwrap_or("").trim();
                        if value == "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==" {
                            auth_seen.store(true, AtomicOrdering::SeqCst);
                        }
                    }
                }
            }

            // Return 400 to avoid dealing with actual TLS tunneling.
            // This will cause an error on the client side, which is expected.
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .build()
        .unwrap();

    // HTTPS request triggers CONNECT tunnel
    let result = client
        .get("https://hyper.rs.local/prox")
        .unwrap()
        .send()
        .await;

    // The request should fail because our mock proxy returns 400
    assert!(result.is_err(), "expected tunnel error, got success");

    assert!(
        auth_seen.load(AtomicOrdering::SeqCst),
        "CONNECT request should include Proxy-Authorization header"
    );
}

#[tokio::test]
async fn test_connect_tunnel_detects_auth_required() {
    let proxy_addr = raw_server(|req_bytes| async move {
        let req_str = String::from_utf8_lossy(&req_bytes);

        if req_str.starts_with("CONNECT") {
            // Return 407 Proxy Authentication Required
            b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n".to_vec()
        } else {
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    // HTTPS request triggers CONNECT tunnel, which should fail with 407
    let err = client
        .get("https://hyper.rs.local/prox")
        .unwrap()
        .send()
        .await;

    assert!(err.is_err(), "expected error from 407 proxy response");
    let err_msg = format!("{}", err.unwrap_err());
    assert!(
        err_msg.contains("407") || err_msg.contains("CONNECT tunnel failed"),
        "expected tunnel failure message, got: {err_msg}"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn test_proxy_settings_routes_http_and_https_separately() {
    let (http_target_addr, _http_counter) = h1_server().await;
    let (https_target_addr, https_cert, _https_counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;

    let http_connects = captured_connects();
    let https_connects = captured_connects();
    let (http_proxy_addr, _http_proxy_conns) =
        connect_proxy_with_capture(Some(http_connects.clone())).await;
    let (https_proxy_addr, _https_proxy_conns) =
        connect_proxy_with_capture(Some(https_connects.clone())).await;

    let settings = aioduct::ProxySettings::default()
        .http(aioduct::ProxyConfig::http(&format!("http://{http_proxy_addr}")).unwrap())
        .https(aioduct::ProxyConfig::http(&format!("http://{https_proxy_addr}")).unwrap());

    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&https_cert),
    );

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy_settings(settings)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{http_target_addr}/test"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    let resp = client
        .get(&format!(
            "https://localhost:{}/test",
            https_target_addr.port()
        ))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "hello tls");

    let http_reqs = http_connects.lock().unwrap();
    assert!(
        http_reqs
            .iter()
            .any(|req| connect_target(req) == http_target_addr.to_string()),
        "HTTP proxy should receive HTTP target CONNECT, got: {http_reqs:?}"
    );
    assert!(
        !http_reqs
            .iter()
            .any(|req| connect_target(req) == format!("localhost:{}", https_target_addr.port())),
        "HTTP proxy should not receive HTTPS target CONNECT, got: {http_reqs:?}"
    );
    drop(http_reqs);

    let https_reqs = https_connects.lock().unwrap();
    assert!(
        https_reqs
            .iter()
            .any(|req| connect_target(req) == format!("localhost:{}", https_target_addr.port())),
        "HTTPS proxy should receive HTTPS target CONNECT, got: {https_reqs:?}"
    );
    assert!(
        !https_reqs
            .iter()
            .any(|req| connect_target(req) == http_target_addr.to_string()),
        "HTTPS proxy should not receive HTTP target CONNECT, got: {https_reqs:?}"
    );
}

#[tokio::test]
async fn test_connect_tunnel_target_authority() {
    // Verify the CONNECT request targets the correct host:port
    let connect_target = Arc::new(std::sync::Mutex::new(String::new()));
    let connect_target_clone = connect_target.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let connect_target = connect_target_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);
            if req_str.starts_with("CONNECT") {
                // Parse "CONNECT host:port HTTP/1.1"
                if let Some(target) = req_str.split_whitespace().nth(1) {
                    *connect_target.lock().unwrap() = target.to_string();
                }
            }
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let _ = client
        .get("https://hyper.rs.local:8443/path")
        .unwrap()
        .send()
        .await;

    let target = connect_target.lock().unwrap().clone();
    assert_eq!(
        target, "hyper.rs.local:8443",
        "CONNECT should target the original host:port"
    );
}

#[tokio::test]
async fn test_connect_tunnel_default_port() {
    // When no explicit port is given, CONNECT should include :443 for HTTPS.
    let connect_target = Arc::new(std::sync::Mutex::new(String::new()));
    let connect_target_clone = connect_target.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let connect_target = connect_target_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);
            if req_str.starts_with("CONNECT")
                && let Some(target) = req_str.split_whitespace().nth(1)
            {
                *connect_target.lock().unwrap() = target.to_string();
            }
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let _ = client
        .get("https://hyper.rs.local/path")
        .unwrap()
        .send()
        .await;

    let target = connect_target.lock().unwrap().clone();
    assert_eq!(
        target, "hyper.rs.local:443",
        "CONNECT should include port 443 for HTTPS when not explicit in the URL"
    );
}

#[test]
fn test_socks5h_constructor() {
    assert!(
        aioduct::ProxyConfig::socks5h("socks5h://proxy.example.com:1080").is_ok(),
        "socks5h:// should be accepted"
    );
}

#[test]
fn test_socks5h_constructor_rejects_wrong_scheme() {
    assert!(aioduct::ProxyConfig::socks5h("socks5://proxy.example.com:1080").is_err());
    assert!(aioduct::ProxyConfig::socks5h("http://proxy.example.com:1080").is_err());
}

#[test]
fn test_https_proxy_constructor() {
    assert!(
        aioduct::ProxyConfig::https("https://proxy.example.com:443").is_ok(),
        "https:// should be accepted"
    );
}

#[test]
fn test_https_proxy_constructor_rejects_wrong_scheme() {
    assert!(aioduct::ProxyConfig::https("http://proxy.example.com:443").is_err());
    assert!(aioduct::ProxyConfig::https("socks5://proxy.example.com:443").is_err());
}

#[test]
fn test_socks5_constructor_without_port() {
    // Should accept URI without explicit port (defaults to 1080)
    assert!(
        aioduct::ProxyConfig::socks5("socks5://proxy.example.com").is_ok(),
        "socks5:// without port should be accepted"
    );
}

#[test]
fn test_https_proxy_constructor_without_port() {
    // Should accept URI without explicit port (defaults to 443)
    assert!(
        aioduct::ProxyConfig::https("https://proxy.example.com").is_ok(),
        "https:// without port should be accepted"
    );
}

// --- Integration tests ---

/// Serializes env var mutations in integration tests.
static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn system_proxy_integration() {
    let connect_seen = Arc::new(AtomicBool::new(false));
    let connect_seen_clone = connect_seen.clone();

    let proxy_addr = raw_server(move |req_bytes| {
        let connect_seen = connect_seen_clone.clone();
        async move {
            let req_str = String::from_utf8_lossy(&req_bytes);
            if req_str.starts_with("CONNECT") {
                connect_seen.store(true, AtomicOrdering::SeqCst);
            }
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_vec()
        }
    })
    .await;

    let proxy_url = format!("http://{proxy_addr}");

    {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::set_var("HTTP_PROXY", &proxy_url);
            std::env::set_var("HTTPS_PROXY", &proxy_url);
            std::env::remove_var("NO_PROXY");
            std::env::remove_var("no_proxy");
        }
    }

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .system_proxy()
        .build()
        .unwrap();

    let result = client
        .get("https://hyper.rs.local/prox")
        .unwrap()
        .send()
        .await;

    {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe {
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("http_proxy");
            std::env::remove_var("HTTPS_PROXY");
            std::env::remove_var("https_proxy");
        }
    }

    // HTTPS request should trigger a CONNECT tunnel through the proxy
    assert!(
        connect_seen.load(AtomicOrdering::SeqCst),
        "system_proxy should route HTTPS request through proxy CONNECT"
    );
    // The request itself should fail because our raw server returns 400
    assert!(result.is_err(), "expected tunnel to fail with 400");
}

#[tokio::test]
async fn proxy_chain_integration() {
    let (target_addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("target-reached"))))
    })
    .await;

    let captured_connects = captured_connects();
    let (proxy_addr, _conns) = connect_proxy_with_capture(Some(captured_connects.clone())).await;

    let chain = aioduct::ProxyChain::single(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    );

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(chain)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "target-reached");

    let connect_reqs = captured_connects.lock().unwrap();
    assert!(
        connect_reqs
            .iter()
            .any(|req| connect_target(req) == target_addr.to_string()),
        "proxy chain should CONNECT to the target, got: {connect_reqs:?}"
    );
}

#[tokio::test]
async fn two_http_proxy_hops_each_connect_before_origin_bytes() {
    let (target_addr, _) = h1_server().await;
    let first_connects = captured_connects();
    let second_connects = captured_connects();
    let (second_addr, _) = connect_proxy_with_capture(Some(second_connects.clone())).await;
    let (first_addr, _) = connect_proxy_with_capture(Some(first_connects.clone())).await;
    let chain = aioduct::ProxyChain::new(vec![
        aioduct::ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
        aioduct::ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
    ]);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(chain)
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/two-http-hops"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    let first = first_connects.lock().unwrap();
    assert!(
        first
            .iter()
            .any(|request| connect_target(request) == second_addr.to_string()),
        "first proxy did not CONNECT to the second proxy: {first:?}"
    );
    let second = second_connects.lock().unwrap();
    assert!(
        second
            .iter()
            .any(|request| connect_target(request) == target_addr.to_string()),
        "second proxy did not CONNECT to the origin: {second:?}"
    );
}

#[tokio::test]
async fn force_addr_only_overrides_origin_in_two_hop_proxy_chain() {
    let (target_addr, _) = h1_server().await;
    let first_connects = captured_connects();
    let second_connects = captured_connects();
    let (second_addr, _) = connect_proxy_with_capture(Some(second_connects.clone())).await;
    let (first_addr, _) = connect_proxy_with_capture(Some(first_connects.clone())).await;
    let chain = aioduct::ProxyChain::new(vec![
        aioduct::ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
        aioduct::ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
    ]);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(chain)
        .build()
        .unwrap();

    let response = client
        .get("http://unresolvable-force-addr.invalid:1/two-hop-forced")
        .unwrap()
        .force_addr(target_addr)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    let first = first_connects.lock().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(connect_target(&first[0]), second_addr.to_string());
    let second = second_connects.lock().unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(connect_target(&second[0]), target_addr.to_string());
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn two_https_proxy_hops_negotiate_h1_and_connect_plain_origin() {
    let (target_addr, _) = h1_server().await;
    let first_connects = captured_connects();
    let second_connects = captured_connects();
    let (second_addr, second_cert, _) =
        tls_connect_proxy_with_capture(Some(second_connects.clone())).await;
    let (first_addr, first_cert, _) =
        tls_connect_proxy_with_capture(Some(first_connects.clone())).await;
    let connector =
        aioduct::tls::RustlsConnector::new(client_config_trusting(&[first_cert, second_cert]));
    let chain = aioduct::ProxyChain::new(vec![
        aioduct::ProxyConfig::https(&format!("https://localhost:{}", first_addr.port())).unwrap(),
        aioduct::ProxyConfig::https(&format!("https://localhost:{}", second_addr.port())).unwrap(),
    ]);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy_chain(chain)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/two-https-hops"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    let first = first_connects.lock().unwrap();
    assert!(
        first.iter().any(|request| {
            connect_target(request) == format!("localhost:{}", second_addr.port())
        }),
        "first HTTPS proxy did not CONNECT to the second proxy: {first:?}"
    );
    let second = second_connects.lock().unwrap();
    assert!(
        second
            .iter()
            .any(|request| connect_target(request) == target_addr.to_string()),
        "second HTTPS proxy did not CONNECT to the origin: {second:?}"
    );
}

#[cfg(feature = "rustls")]
async fn assert_https_then_socks_chain(second: aioduct::ProxyConfig, expected_second: String) {
    let (target_addr, _) = h1_server().await;
    let first_connects = captured_connects();
    let (first_addr, first_cert, _) =
        tls_connect_proxy_with_capture(Some(first_connects.clone())).await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&first_cert),
    );
    let chain = aioduct::ProxyChain::new(vec![
        aioduct::ProxyConfig::https(&format!("https://localhost:{}", first_addr.port())).unwrap(),
        second,
    ]);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy_chain(chain)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/https-then-socks"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    let first = first_connects.lock().unwrap();
    assert!(
        first
            .iter()
            .any(|request| connect_target(request) == expected_second),
        "first HTTPS proxy did not CONNECT to the SOCKS proxy: {first:?}"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_then_socks5_chain_executes_both_hops() {
    let second_addr = socks5_proxy().await;
    assert_https_then_socks_chain(
        aioduct::ProxyConfig::socks5(&format!("socks5://{second_addr}")).unwrap(),
        second_addr.to_string(),
    )
    .await;
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_then_socks4_chain_executes_both_hops() {
    let second_addr = socks4_proxy().await;
    assert_https_then_socks_chain(
        aioduct::ProxyConfig::socks4(&format!("socks4://{second_addr}")).unwrap(),
        second_addr.to_string(),
    )
    .await;
}

#[cfg(feature = "rustls")]
async fn assert_mixed_chain_send(
    first: mixed_chain::ProxyKind,
    second: mixed_chain::ProxyKind,
    origin_protocol: mixed_chain::OriginProtocol,
) {
    use mixed_chain::{FIRST_PROXY_HOST, LiveMixedProxyChain, ORIGIN_HOST, SECOND_PROXY_HOST};

    let fixture = LiveMixedProxyChain::start_with_origin(first, second, origin_protocol);
    let connector = aioduct::tls::RustlsConnector::new(mixed_chain::client_config_trusting(
        fixture.certificates(),
    ));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy_chain(aioduct::ProxyChain::new(vec![
            fixture.first_proxy(),
            fixture.second_proxy(),
        ]))
        .resolve(FIRST_PROXY_HOST, fixture.first_addr())
        .resolve(SECOND_PROXY_HOST, fixture.second_addr())
        .resolve(ORIGIN_HOST, fixture.origin_addr())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(&fixture.origin_url())
        .unwrap()
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("{first:?} -> {second:?} via {origin_protocol:?} failed: {error}")
        });
    assert_eq!(response.text().await.unwrap(), "mixed-chain-ok");
    fixture.assert_wire_order();
}

#[cfg(feature = "rustls")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_two_hop_proxy_pairs_execute_in_wire_order() {
    let cases = mixed_chain::ordered_proxy_pairs();
    mixed_chain::assert_complete_ordered_pairs(&cases);
    for (first, second) in cases {
        assert_mixed_chain_send(first, second, mixed_chain::OriginProtocol::Http1).await;
    }
}

#[cfg(feature = "rustls")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chained_https_origins_negotiate_h1_and_h2_for_all_proxy_pairs() {
    let cases = mixed_chain::ordered_proxy_pairs();
    mixed_chain::assert_complete_ordered_pairs(&cases);
    assert!(
        cases.contains(&(mixed_chain::ProxyKind::Https, mixed_chain::ProxyKind::Https)),
        "HTTPS origin matrix must include HTTPS -> HTTPS -> HTTPS triple TLS"
    );
    for protocol in [
        mixed_chain::OriginProtocol::HttpsHttp1,
        mixed_chain::OriginProtocol::HttpsHttp2,
    ] {
        for &(first, second) in &cases {
            assert_mixed_chain_send(first, second, protocol).await;
        }
    }
}

#[tokio::test]
async fn invalid_proxy_chain_fails_before_first_network_io() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_addr = listener.local_addr().unwrap();
    let chain = aioduct::ProxyChain::new(vec![
        aioduct::ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
        aioduct::ProxyConfig::http("http://127.0.0.1:9").unwrap(),
        aioduct::ProxyConfig::http("http://127.0.0.1:10").unwrap(),
    ]);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(chain)
        .build()
        .unwrap();

    let error = client
        .get("http://127.0.0.1:11/preflight")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(error.to_string().contains("longer than 2 hops"), "{error}");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "invalid proxy plan opened a network connection"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn exact_h3_proxy_route_fails_before_first_network_io() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let error = client
        .forward(
            hyper::Request::builder()
                .uri("/ingress")
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .upstream("https://127.0.0.1:9".parse::<http::Uri>().unwrap())
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("HTTP/3 through a proxy"),
        "{error}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "unsupported HTTP/3 proxy route opened a network connection"
    );
}

#[tokio::test]
async fn h2c_prior_knowledge_survives_connect_tunnel() {
    let (target_addr, _) = aioduct_test_server::h2::h2_server().await;
    let (proxy_addr, _) = connect_proxy().await;
    let (phase_tx, mut phase_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(LiveProxyPhaseObserver(phase_tx))
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/h2c-through-proxy"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();

    assert_eq!(response.version(), http::Version::HTTP_2);
    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert_eq!(
        phase_rx.try_recv().unwrap(),
        LiveProxyPhase::Tcp(aioduct::NegotiatedProtocol::Http1)
    );
    assert!(phase_rx.try_recv().is_err());
}

#[tokio::test]
async fn adaptive_h2c_probes_h2_through_connect_tunnel_and_caches_route() {
    let (target_addr, _) = aioduct_test_server::h2::h2_server().await;
    let (proxy_addr, proxy_connections) = connect_proxy().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();
    let upstream = format!("http://{target_addr}")
        .parse::<http::Uri>()
        .unwrap();

    for path in ["/first", "/cached"] {
        let response = client
            .forward(
                hyper::Request::builder()
                    .uri(path)
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .upstream(upstream.clone())
            .adaptive_h2c()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    }

    assert_eq!(
        proxy_connections.load(AtomicOrdering::SeqCst),
        1,
        "the cached H2 tunnel should be reused"
    );
}

#[tokio::test]
async fn adaptive_h2c_reconnects_same_proxy_route_for_h1_fallback() {
    let (target_addr, _) = h1_server().await;
    let (proxy_addr, proxy_connections) = connect_proxy().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();
    let upstream = format!("http://{target_addr}")
        .parse::<http::Uri>()
        .unwrap();

    for path in ["/fallback", "/cached"] {
        let response = client
            .forward(
                hyper::Request::builder()
                    .uri(path)
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .upstream(upstream.clone())
            .adaptive_h2c()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    }

    assert_eq!(
        proxy_connections.load(AtomicOrdering::SeqCst),
        2,
        "one H2 probe tunnel and one cached H1 fallback tunnel are expected"
    );
}

#[tokio::test]
async fn adaptive_h2c_times_out_silent_settings_through_proxy() {
    let target_addr = silent_h2c_then_h1_origin().await;
    let (proxy_addr, proxy_connections) = connect_proxy().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        client
            .forward(
                hyper::Request::builder()
                    .uri("/silent-settings")
                    .header(http::header::HOST, "downstream.test")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .upstream(format!("http://{target_addr}"))
            .adaptive_h2c()
            .send(),
    )
    .await
    .expect("silent HTTP/2 SETTINGS probe did not fall back")
    .unwrap();

    assert_eq!(response.text().await.unwrap(), "h1 fallback");
    assert_eq!(
        proxy_connections.load(AtomicOrdering::SeqCst),
        2,
        "the timed-out H2 probe must reconnect through the same proxy for H1"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn unknown_origin_alpn_is_rejected_inside_connect_tunnel_before_http_bytes() {
    use tokio::io::AsyncReadExt as _;

    aioduct_test_server::tls::install_crypto_provider();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    let mut server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der)
            .unwrap();
    server_config.alpn_protocols = vec![b"custom-proto".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = listener.local_addr().unwrap();
    let (read_tx, read_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = acceptor.accept(stream).await.unwrap();
        let mut byte = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte)).await;
        let _ = read_tx.send(read);
    });

    let (proxy_addr, _) = connect_proxy().await;
    let mut client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    Arc::get_mut(&mut client_config).unwrap().alpn_protocols = vec![b"custom-proto".to_vec()];
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let error = client
        .get(&format!(
            "https://localhost:{}/unknown-alpn",
            target_addr.port()
        ))
        .unwrap()
        .send()
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("custom-proto"),
        "unexpected tunneled unknown-ALPN error: {error}"
    );
    let read = read_rx.await.unwrap();
    assert!(
        !matches!(read, Ok(Ok(written)) if written != 0),
        "client sent HTTP bytes through CONNECT after unknown ALPN: {read:?}"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn adaptive_h2c_uses_normal_alpn_for_proxied_https_h1_origin() {
    let (target_addr, target_cert, _) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let (proxy_addr, proxy_connections) = connect_proxy().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&target_cert),
    );
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let response = client
        .forward(
            hyper::Request::builder()
                .uri("/proxied-https-h1")
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .upstream(
            format!("https://localhost:{}", target_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.version(), http::Version::HTTP_11);
    assert_eq!(response.text().await.unwrap(), "hello tls");
    assert_eq!(
        proxy_connections.load(AtomicOrdering::SeqCst),
        1,
        "proxied HTTPS must use one ALPN-negotiated tunnel, not an h2c probe and fallback"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn adaptive_h2c_uses_normal_alpn_for_proxied_https_h2_origin() {
    let (target_addr, target_cert, _) = aioduct_test_server::tls::tls_h2_server().await;
    let (proxy_addr, proxy_connections) = connect_proxy().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&target_cert),
    );
    let observer = TlsAlpnObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(observer.clone())
        .build()
        .unwrap();

    let response = client
        .forward(
            hyper::Request::builder()
                .uri("/proxied-https-h2")
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .upstream(
            format!("https://localhost:{}", target_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .adaptive_h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "hello tls");
    assert_eq!(observer.negotiated_protocols(), [Some("h2".to_owned())]);
    assert_eq!(proxy_connections.load(AtomicOrdering::SeqCst), 1);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn required_h2_alpn_survives_https_origin_tunnel() {
    let (target_addr, target_cert, _) = aioduct_test_server::tls::tls_h2_server().await;
    let (proxy_addr, _) = connect_proxy().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&target_cert),
    );
    let observer = TlsAlpnObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .request_observer(observer.clone())
        .build()
        .unwrap();

    let response = client
        .get(&format!(
            "https://localhost:{}/h2-through-proxy",
            target_addr.port()
        ))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();

    assert_eq!(response.version(), http::Version::HTTP_2);
    assert_eq!(response.text().await.unwrap(), "hello tls");
    assert_eq!(observer.negotiated_protocols(), [Some("h2".to_owned())]);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forwarded_exact_h1_and_h2_survive_https_origin_tunnel() {
    let (target_addr, target_cert) = negotiated_tls_server().await;
    let (proxy_addr, _) = connect_proxy().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&target_cert),
    );
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(connector)
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();
    let upstream = format!("https://localhost:{}", target_addr.port())
        .parse::<http::Uri>()
        .unwrap();

    let exact_h1 = client
        .forward(
            hyper::Request::builder()
                .uri("/ingress")
                .version(http::Version::HTTP_2)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .upstream(upstream.clone())
        .on_request(|parts| parts.version = http::Version::HTTP_11)
        .send()
        .await
        .unwrap();
    assert_eq!(exact_h1.version(), http::Version::HTTP_2);
    assert_eq!(exact_h1.text().await.unwrap(), "HTTP/1.1");

    let exact_h2 = client
        .forward(
            hyper::Request::builder()
                .uri("/ingress")
                .version(http::Version::HTTP_11)
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .upstream(upstream)
        .on_request(|parts| parts.version = http::Version::HTTP_2)
        .send()
        .await
        .unwrap();
    assert_eq!(exact_h2.version(), http::Version::HTTP_11);
    assert_eq!(exact_h2.text().await.unwrap(), "HTTP/2.0");
}

#[tokio::test]
async fn no_proxy_cidr_integration() {
    // Target server on localhost
    let (target_addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    // A "proxy" server that labels responses
    let (proxy_addr, _) = h1_server_with(|req| async move {
        let uri = req.uri().to_string();
        let body = format!("proxied: {uri}");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
    })
    .await;

    // NoProxy with CIDR 127.0.0.0/8 — covers all localhost IPs
    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new("127.0.0.0/8"));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    // Request to localhost target should bypass the proxy
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "direct");
}

#[tokio::test]
async fn no_proxy_port_specific() {
    let (target_addr, _) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("direct"))))
    })
    .await;

    let captured_connects = captured_connects();
    let (proxy_addr, _) = connect_proxy_with_capture(Some(captured_connects.clone())).await;

    let target_ip = target_addr.ip().to_string();
    let non_matching_port = if target_addr.port() == u16::MAX {
        target_addr.port() - 1
    } else {
        target_addr.port() + 1
    };
    let no_proxy_rule = format!("{target_ip}:{non_matching_port}");

    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new(&no_proxy_rule));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "direct");
    assert!(
        captured_connects
            .lock()
            .unwrap()
            .iter()
            .any(|req| connect_target(req) == target_addr.to_string()),
        "port mismatch should use proxy CONNECT"
    );

    let before_matching_rule = captured_connects.lock().unwrap().len();
    let settings = aioduct::ProxySettings::all(
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap(),
    )
    .no_proxy(aioduct::NoProxy::new(&format!(
        "{target_ip}:{}",
        target_addr.port()
    )));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "direct");
    assert_eq!(
        captured_connects.lock().unwrap().len(),
        before_matching_rule,
        "matching host:port no_proxy rule should bypass the proxy"
    );
}

#[tokio::test]
async fn proxy_failure_dns() {
    // Use a hostname that will never resolve to trigger DNS failure
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(
            aioduct::ProxyConfig::http("http://this.hostname.does.not.exist.invalid:80").unwrap(),
        )
        .build()
        .unwrap();

    let err = client
        .get("http://example.com/path")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        err.is_dns(),
        "expected DNS error, got: {err} (is_dns={})",
        err.is_dns()
    );
}

#[tokio::test]
async fn proxy_failure_connection_refused() {
    // Bind a port, get its address, then drop the listener so the port is closed
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{addr}")).unwrap())
        .build()
        .unwrap();

    let err = client
        .get("http://example.com/path")
        .unwrap()
        .send()
        .await
        .unwrap_err();

    assert!(
        err.is_connect(),
        "expected connect error, got: {err} (is_connect={})",
        err.is_connect()
    );
}

// ── Proxy edge-case tests ─────────────────────────────────────────────

#[tokio::test]
async fn proxy_settings_custom_with_no_proxy_precedence() {
    // Verify no_proxy is checked before custom (settings.rs:96).
    let (proxy_addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("proxied"))))
    })
    .await;

    let (target_addr, _counter) = h1_server().await;

    let called = Arc::new(AtomicBool::new(false));
    let called2 = called.clone();

    let settings = aioduct::ProxySettings::default()
        .custom(move |_url| {
            called2.store(true, AtomicOrdering::SeqCst);
            Some(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        })
        .no_proxy(aioduct::NoProxy::new("127.0.0.1"));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    // Target is localhost — no_proxy matches, custom should NOT be called.
    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello aioduct");
    assert!(!called.load(AtomicOrdering::SeqCst));
}

#[tokio::test]
async fn custom_proxy_is_resolved_once_per_dispatch_attempt() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let observed_resolutions = resolutions.clone();

    let settings = aioduct::ProxySettings::default().custom(move |_uri| {
        observed_resolutions.fetch_add(1, AtomicOrdering::SeqCst);
        Some(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
    });
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/custom-proxy"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert_eq!(resolutions.load(AtomicOrdering::SeqCst), 1);
}

#[tokio::test]
async fn rotating_proxy_selection_binds_credentials_pool_and_transport() {
    let (target_addr, _) = h1_server().await;
    let first_connects = captured_connects();
    let second_connects = captured_connects();
    let (first_addr, first_connections) =
        connect_proxy_with_capture(Some(first_connects.clone())).await;
    let (second_addr, second_connections) =
        connect_proxy_with_capture(Some(second_connects.clone())).await;

    let selector_calls = Arc::new(AtomicUsize::new(0));
    let observed_selector_calls = selector_calls.clone();
    let first = aioduct::ProxyConfig::http(&format!("http://{first_addr}")).unwrap();
    let second = aioduct::ProxyConfig::http(&format!("http://{second_addr}")).unwrap();

    struct CountingResolver(Arc<AtomicUsize>);
    impl aioduct::CredentialResolver for CountingResolver {
        fn resolve(&self, _key: &str) -> Option<(String, String)> {
            self.0.fetch_add(1, AtomicOrdering::SeqCst);
            Some(("Aladdin".to_owned(), "open sesame".to_owned()))
        }
    }
    let resolver_calls = Arc::new(AtomicUsize::new(0));

    let settings = aioduct::ProxySettings::default()
        .custom(move |_uri| {
            let call = observed_selector_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Some(if call.is_multiple_of(2) {
                first.clone()
            } else {
                second.clone()
            })
        })
        .proxy_credential_resolver(CountingResolver(resolver_calls.clone()));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    for _ in 0..3 {
        let response = client
            .get(&format!("http://{target_addr}/snapshot"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    }

    assert_eq!(selector_calls.load(AtomicOrdering::SeqCst), 3);
    assert_eq!(resolver_calls.load(AtomicOrdering::SeqCst), 3);
    assert_eq!(first_connections.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(second_connections.load(AtomicOrdering::SeqCst), 1);
    assert_connect_for_target_has_auth(&first_connects, &target_addr.to_string());
    assert_connect_for_target_has_auth(&second_connects, &target_addr.to_string());
}

#[tokio::test]
async fn canonical_proxy_routes_share_pool_identity() {
    let (target_addr, _) = h1_server().await;
    let captured = captured_connects();
    let (proxy_addr, proxy_connections) = connect_proxy_with_capture(Some(captured.clone())).await;
    let selector_calls = Arc::new(AtomicUsize::new(0));
    let observed_selector_calls = selector_calls.clone();
    let first = aioduct::ProxyConfig::http(&format!(
        "http://old:credentials@{proxy_addr}/ignored?route=first"
    ))
    .unwrap()
    .basic_auth("Aladdin", "open sesame");
    let second =
        aioduct::ProxyConfig::http(&format!("http://{proxy_addr}/another-path?route=second"))
            .unwrap()
            .basic_auth("Aladdin", "open sesame");
    let settings = aioduct::ProxySettings::default().custom(move |_uri| {
        let call = observed_selector_calls.fetch_add(1, AtomicOrdering::SeqCst);
        Some(if call.is_multiple_of(2) {
            first.clone()
        } else {
            second.clone()
        })
    });
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_settings(settings)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .unwrap();

    for path in ["first", "second", "third"] {
        let response = client
            .get(&format!("http://{target_addr}/{path}"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    }

    assert_eq!(selector_calls.load(AtomicOrdering::SeqCst), 3);
    assert_eq!(proxy_connections.load(AtomicOrdering::SeqCst), 1);
    assert_connect_for_target_has_auth(&captured, &target_addr.to_string());
}

#[tokio::test]
async fn proxy_pool_diagnostics_use_engine_scoped_opaque_route_labels() {
    let (target_addr, _) = h1_server().await;
    let (proxy_addr, _) = connect_proxy().await;
    let proxy = aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
        .unwrap()
        .basic_auth("diagnostic-user", "diagnostic-password")
        .header(
            http::header::HeaderName::from_static("x-route-token"),
            http::HeaderValue::from_static("diagnostic-secret"),
        );

    let first = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(proxy.clone())
        .build()
        .unwrap();
    let second = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(proxy)
        .build()
        .unwrap();

    for client in [&first, &second] {
        let response = client
            .get(&format!("http://{target_addr}/opaque-route"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "hello aioduct");
    }

    let first_label = first.pool_stats().hosts[0].route.clone();
    assert_eq!(first.pool_stats().hosts[0].route, first_label);
    let second_label = second.pool_stats().hosts[0].route.clone();

    assert!(first_label.starts_with("proxy-"), "{first_label}");
    assert_ne!(first_label, second_label);
    for secret in [
        "diagnostic-user",
        "diagnostic-password",
        "diagnostic-secret",
    ] {
        assert!(!first_label.contains(secret));
        assert!(!second_label.contains(secret));
    }
}

#[tokio::test]
async fn proxy_with_redirect_routing() {
    // Target server (behind proxy).
    let (target_addr, _counter) = h1_server().await;

    // Server that redirects to the target.
    let (redirect_addr, _counter) = h1_server_with(move |_req| {
        let target = format!("http://{target_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    // Raw TCP proxy that counts connections and forwards to the redirect server.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let proxy_req_count = Arc::new(AtomicUsize::new(0));
    let prc = Arc::clone(&proxy_req_count);

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            prc.fetch_add(1, AtomicOrdering::SeqCst);
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            // Reply with a redirect to the target — the redirect follow-up
            // will also go through the proxy.
            let target = format!("http://{target_addr}/");
            let resp =
                format!("HTTP/1.1 302 Found\r\nlocation: {target}\r\nContent-Length: 0\r\n\r\n");
            stream.write_all(resp.as_bytes()).await.ok();
        }
    });

    // Client configured with the proxy.
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    // Send request to the redirect server through proxy.
    // The redirect server redirects to target; the redirect target also
    // goes through the proxy which redirects again to target (infinite loop).
    // The client will exhaust max_redirects. We verify the proxy saw
    // the request.
    let result = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await;

    // The redirect chain will exhaust max_redirects because the proxy always
    // issues 302 back to the target.
    assert!(
        result.is_err(),
        "redirect loop should exhaust max_redirects"
    );

    // Proxy saw at least the initial request.
    let count = proxy_req_count.load(AtomicOrdering::SeqCst);
    assert!(
        count >= 1,
        "proxy should see the initial request, got {count}"
    );
}

#[tokio::test]
async fn credential_resolver_global_env() {
    // EnvCredentialResolver applies global credentials (ignores key).
    use aioduct::{CredentialResolver, EnvCredentialResolver};

    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_MUTEX.lock().unwrap();

    // The resolver reads from env; using defaults means no proxy-user is set.
    // The resolver is a no-op when no env vars are set.
    // Clear env vars that might have been inherited from the test process.
    // ENV_MUTEX serializes proxy env tests; remove_var is unsafe in Rust 2024.
    unsafe {
        std::env::remove_var("AIODUCT_PROXY_USER");
        std::env::remove_var("AIODUCT_PROXY_PASS");
    }

    let resolver = EnvCredentialResolver;
    let result = resolver.resolve("any-key");
    // With no env vars set, resolver returns None (no credentials).
    assert!(result.is_none());
}

#[tokio::test]
async fn proxy_connection_pooling_with_counter() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, conn_count) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .build()
        .unwrap();

    for _ in 0..2 {
        let resp = client
            .get(&format!("http://{target_addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.bytes().await.unwrap();
    }

    let count = conn_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(count, 1, "same target should reuse the CONNECT tunnel");
}

#[tokio::test]
async fn proxy_connection_pooling_keeps_targets_separate() {
    let (first_addr, _first_counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("first"))))
    })
    .await;
    let (second_addr, _second_counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("second"))))
    })
    .await;
    let (proxy_addr, conn_count) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{first_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "first");

    let resp = client
        .get(&format!("http://{second_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "second");

    let count = conn_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(count, 2, "different targets should use distinct tunnels");
}

#[tokio::test]
async fn proxy_failure_reset_deterministic() {
    // Raw TCP server that accepts then immediately closes (RST).
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // Immediately drop — sends RST, no HTTP response.
        drop(stream);
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{addr}")).unwrap())
        .build()
        .unwrap();

    let result = client.get("http://example.com/path").unwrap().send().await;

    assert!(result.is_err(), "proxy reset should produce an error");
}

// ── CONNECT tunnel proxy tests ────────────────────────────────────────────

/// HTTP proxy for HTTP target now uses CONNECT tunnel.
/// Proxy and target are separate servers — the proxy relays bytes.
#[tokio::test]
async fn http_proxy_uses_connect_tunnel() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert!(conns.load(AtomicOrdering::SeqCst) >= 1);
}

#[tokio::test]
async fn http_proxy_tunnel_applies_tcp_keepalive_to_proxy_connection() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, _conns) = connect_proxy().await;

    let connector = ProxyKeepaliveCountingConnector::new();
    let connector_ref = connector.clone();

    let client =
        HttpEngineSend::<TokioRuntime, ProxyKeepaliveCountingConnector>::builder_with_connector(
            connector,
        )
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    assert_eq!(
        connector_ref.keepalive_calls(),
        1,
        "configured tcp_keepalive should apply to the proxy tunnel TCP stream"
    );
}

/// Two sequential requests through the same CONNECT tunnel both succeed.
#[tokio::test]
async fn connect_tunnel_pooled_reuse() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, conns) = connect_proxy().await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(5)
        .build()
        .unwrap();

    for _ in 0..2 {
        let resp = client
            .get(&format!("http://{target_addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    }
    assert!(conns.load(AtomicOrdering::SeqCst) >= 1);
}

/// End-to-end TLS-to-proxy: a plaintext HTTP target reached through an
/// `https://` proxy. This forces the client to perform a real TLS handshake
/// to the proxy (SNI = proxy host) and then CONNECT-tunnel over that encrypted
/// channel. Verifies `ProxyConfig::https()` actually works against a live TLS
/// proxy, not just that it parses.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_tls_to_proxy_reaches_http_target() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, proxy_cert, conns) = tls_connect_proxy().await;

    // Client must trust the proxy's self-signed cert. The proxy URL uses
    // `localhost` (the cert's SAN), so SNI verification to the proxy passes.
    let client_config = aioduct_test_server::tls::make_client_config(&proxy_cert);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    assert!(
        conns.load(AtomicOrdering::SeqCst) >= 1,
        "the TLS proxy should have accepted at least one connection"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_tls_does_not_receive_origin_client_identity() {
    aioduct_test_server::tls::install_crypto_provider();
    let (target_addr, _) = h1_server().await;
    let client_certificate =
        rcgen::generate_simple_self_signed(vec!["origin-client.test".into()]).unwrap();
    let client_certificate_der =
        rustls::pki_types::CertificateDer::from(client_certificate.cert.der().to_vec());
    let mut identity_pem = client_certificate.cert.pem();
    identity_pem.push_str(&client_certificate.signing_key.serialize_pem());
    let identity = aioduct::tls::Identity::from_pem(identity_pem.as_bytes()).unwrap();
    let (proxy_addr, proxy_certificate, client_certificate_seen) =
        tls_connect_proxy_observing_client_certificate(client_certificate_der).await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .add_root_certificates(&[aioduct::tls::Certificate::from_der(
            proxy_certificate.to_vec(),
        )])
        .identity(identity)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/origin-identity"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert!(
        !client_certificate_seen.load(AtomicOrdering::SeqCst),
        "origin client identity leaked to the HTTPS proxy"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_without_alpn_defaults_to_http1_connect() {
    let (target_addr, _counter) = h1_server().await;
    let (proxy_addr, proxy_cert, conns) = tls_connect_proxy_without_alpn().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&proxy_cert),
    );

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(&format!("http://{target_addr}/no-alpn"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "hello aioduct");
    assert_eq!(conns.load(AtomicOrdering::SeqCst), 1);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_rejects_h2_only_endpoint_before_connect_bytes() {
    let (proxy_addr, proxy_cert, application_data_seen) = tls_h2_only_proxy().await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&proxy_cert),
    );
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let result = client
        .get("http://127.0.0.1:9/unreachable")
        .unwrap()
        .send()
        .await;

    assert!(result.is_err(), "an H2-only proxy must be rejected");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !application_data_seen.load(AtomicOrdering::SeqCst),
        "textual CONNECT bytes were sent after non-HTTP/1.1 proxy negotiation"
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_http_target_connect_includes_proxy_auth() {
    let (target_addr, _counter) = h1_server().await;
    let captured = captured_connects();
    let (proxy_addr, proxy_cert, _conns) =
        tls_connect_proxy_with_capture(Some(captured.clone())).await;

    let client_config = aioduct_test_server::tls::make_client_config(&proxy_cert);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/path"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    assert_connect_for_target_has_auth(&captured, &target_addr.to_string());
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_https_target_connect_includes_proxy_auth() {
    let (origin_addr, origin_cert, _origin_counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let captured = captured_connects();
    let (proxy_addr, proxy_cert, _conns) =
        tls_connect_proxy_with_capture(Some(captured.clone())).await;

    let client_config = client_config_trusting(&[proxy_cert, origin_cert]);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", origin_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello tls");

    assert_connect_for_target_has_auth(&captured, &format!("localhost:{}", origin_addr.port()));
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn http_proxy_auth_survives_http_to_https_redirect() {
    let (target_addr, target_cert, _target_counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let location = format!("https://localhost:{}/final", target_addr.port());
    let (redirect_addr, _redirect_counter) = h1_server_with(move |_req| {
        let location = location.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(http::StatusCode::FOUND)
                    .header(http::header::LOCATION, location)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;

    let captured = captured_connects();
    let (proxy_addr, _conns) = connect_proxy_with_capture(Some(captured.clone())).await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&target_cert),
    );

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{redirect_addr}/start"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello tls");

    assert_connect_for_target_has_auth(&captured, &redirect_addr.to_string());
    assert_connect_for_target_has_auth(&captured, &format!("localhost:{}", target_addr.port()));
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn http_proxy_auth_survives_https_to_http_redirect() {
    let (target_addr, _target_counter) = h1_server().await;
    let location = format!("http://{target_addr}/final");
    let (redirect_addr, redirect_cert, _redirect_counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], move |_req| {
            let location = location.clone();
            async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(http::StatusCode::FOUND)
                        .header(http::header::LOCATION, location)
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            }
        })
        .await;

    let captured = captured_connects();
    let (proxy_addr, _conns) = connect_proxy_with_capture(Some(captured.clone())).await;
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&redirect_cert),
    );

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::http(&format!("http://{proxy_addr}"))
                .unwrap()
                .basic_auth("Aladdin", "open sesame"),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/start", redirect_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");

    assert_connect_for_target_has_auth(&captured, &format!("localhost:{}", redirect_addr.port()));
    assert_connect_for_target_has_auth(&captured, &target_addr.to_string());
}

/// End-to-end double-TLS: an HTTPS target reached through an `https://` proxy.
/// This is the other branch of the HTTPS-proxy connect logic
/// (`connect_tunnel_send`): client→proxy TLS, then HTTP CONNECT over that pipe,
/// then a second client→origin TLS handshake *inside* the tunnel. The proxy
/// relays raw bytes, so the inner origin TLS is opaque to it. The single client
/// connector must trust both the proxy cert and the origin cert.
#[cfg(feature = "rustls")]
#[tokio::test]
async fn https_proxy_tls_to_proxy_reaches_https_target() {
    let (origin_addr, origin_cert, _origin_counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let (proxy_addr, proxy_cert, conns) = tls_connect_proxy().await;

    // One connector trusting both the proxy and the origin certs.
    let client_config = client_config_trusting(&[proxy_cert, origin_cert]);
    let connector = aioduct::tls::RustlsConnector::new(client_config);

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .proxy(
            aioduct::ProxyConfig::https(&format!("https://localhost:{}", proxy_addr.port()))
                .unwrap(),
        )
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    // The origin uses a `localhost` cert, so the CONNECT target and origin SNI
    // must be `localhost` for verification to pass inside the tunnel.
    let resp = client
        .get(&format!("https://localhost:{}/", origin_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello tls");
    assert!(
        conns.load(AtomicOrdering::SeqCst) >= 1,
        "the TLS proxy should have accepted at least one connection"
    );
}

/// When HTTP/3 is enabled on a client that also has a proxy configured, the
/// proxy must not be bypassed: the client must still tunnel through the proxy
/// rather than attempt a direct HTTP/3 connection to the origin.
#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn http3_with_proxy_uses_connect_tunnel() {
    aioduct_test_server::tls::install_crypto_provider();

    let (origin_addr, origin_cert, _origin_counter) =
        aioduct_test_server::tls::tls_h1_server(&[b"http/1.1"]).await;
    let captured = captured_connects();
    let (proxy_addr, _conns) = connect_proxy_with_capture(Some(captured.clone())).await;

    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&origin_cert),
    );

    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .http3(true)
        .unwrap()
        .proxy(aioduct::ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("https://localhost:{}/", origin_addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello tls");

    let connect_reqs = captured.lock().unwrap();
    assert!(
        connect_reqs
            .iter()
            .any(|req| connect_target(req) == format!("localhost:{}", origin_addr.port())),
        "proxy CONNECT tunnel should be used, got: {connect_reqs:?}"
    );
}

/// Two-hop chain: SOCKS5 (first hop) → HTTP second hop.  The second hop
/// requires its own CONNECT before relaying to the origin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_chain_socks_then_http_connect() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (target_addr, _counter) = h1_server().await;
    let second_connects = captured_connects();
    let (second_addr, _second_connections) =
        connect_proxy_with_capture(Some(second_connects.clone())).await;

    // SOCKS5 mock: after SOCKS5 handshake, connects to second hop and relays.
    let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socks_addr = socks_listener.local_addr().unwrap();
    let second = second_addr;

    tokio::spawn(async move {
        let (mut client, _) = socks_listener.accept().await.unwrap();
        let mut buf = [0u8; 512];
        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 3 && buf[0] == 0x05);
        client.write_all(&[0x05, 0x00]).await.unwrap();

        let n = client.read(&mut buf).await.unwrap();
        assert!(n >= 7 && buf[0] == 0x05 && buf[1] == 0x01);

        let mut upstream = tokio::net::TcpStream::connect(second).await.unwrap();
        client
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();

        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });

    let chain = aioduct::ProxyChain::new(vec![
        aioduct::ProxyConfig::socks5(&format!("socks5://{socks_addr}")).unwrap(),
        aioduct::ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
    ]);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .proxy_chain(chain)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{target_addr}/through-chain"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    let requests = second_connects.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|request| connect_target(request) == target_addr.to_string()),
        "second proxy did not CONNECT to the origin: {requests:?}"
    );
}
