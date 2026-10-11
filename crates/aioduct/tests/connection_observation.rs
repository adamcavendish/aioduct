#![cfg(all(not(target_arch = "wasm32"), feature = "tokio"))]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

#[cfg(feature = "compio")]
use aioduct::{HttpClient, RequestBuilderExt};

use aioduct::runtime::tokio_rt::{TcpConnector, TokioRuntime};
use aioduct::{
    ConnectionAcquisitionEvent, ConnectionAcquisitionPhase, ConnectionObservation,
    ConnectionObservationEvent, Interest, NoopRequestObservation, Observer, RequestInfo,
    RequestObservation,
};
use bytes::Bytes;
use http_body_util::Full;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fact {
    Established(http::Version),
}

#[derive(Default)]
struct Records {
    begin_connections: Mutex<usize>,
    facts: Mutex<Vec<Fact>>,
    establishment: Mutex<Vec<EstablishmentFact>>,
    physical_terminals: Mutex<Vec<(usize, EstablishmentFact)>>,
    acquisitions: Mutex<Vec<(u64, ConnectionAcquisitionPhase, AcquisitionFact)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcquisitionFact {
    Started,
    Completed,
    Failed(bool),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EstablishmentFact {
    Failed(bool),
    Cancelled,
    Closed,
}

#[derive(Clone, Copy, Debug)]
enum StalledProxy {
    Http,
    Socks4,
    Socks5,
    #[cfg(feature = "rustls")]
    Https,
}

impl StalledProxy {
    fn stalled_phase(self) -> ConnectionAcquisitionPhase {
        match self {
            #[cfg(feature = "rustls")]
            Self::Https => ConnectionAcquisitionPhase::TlsHandshake,
            _ => ConnectionAcquisitionPhase::ProxyConnect,
        }
    }
    fn cases() -> Vec<Self> {
        vec![
            Self::Http,
            Self::Socks4,
            Self::Socks5,
            #[cfg(feature = "rustls")]
            Self::Https,
        ]
    }

    fn config(self, address: std::net::SocketAddr) -> aioduct::ProxyConfig {
        match self {
            Self::Http => aioduct::ProxyConfig::http(&format!("http://{address}")),
            Self::Socks4 => aioduct::ProxyConfig::socks4(&format!("socks4a://{address}")),
            Self::Socks5 => aioduct::ProxyConfig::socks5h(&format!("socks5h://{address}")),
            #[cfg(feature = "rustls")]
            Self::Https => aioduct::ProxyConfig::https(&format!("https://{address}")),
        }
        .unwrap()
    }
}

async fn stalled_proxy() -> (
    std::net::SocketAddr,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let server_started = started.clone();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        server_started.notify_one();
        std::future::pending::<()>().await;
    });
    (address, started, task)
}

#[tokio::test]
async fn proxy_deadline_reports_failure_and_user_cancellation_reports_cancellation() {
    for proxy in StalledProxy::cases() {
        for cancelled in [false, true] {
            let (address, started, server) = stalled_proxy().await;
            let records = Arc::new(Records::default());
            let builder = aioduct::TokioClient::builder()
                .observer(TestObserver(records.clone()))
                .proxy(proxy.config(address))
                .connect_timeout(std::time::Duration::from_millis(80));
            #[cfg(feature = "rustls")]
            let builder = builder.tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs());
            let client = builder.build().unwrap();
            let mut send = Box::pin(client.get("http://origin.test/").unwrap().send());
            if cancelled {
                tokio::select! {
                    result = &mut send => panic!("{proxy:?}: unexpected request result {result:?}"),
                    () = started.notified() => {},
                }
                drop(send);
            } else {
                assert!(
                    matches!(
                        send.await.unwrap_err().into_error(),
                        aioduct::Error::ConnectTimeout
                    ),
                    "{proxy:?}"
                );
            }
            assert_eq!(*records.begin_connections.lock().unwrap(), 1, "{proxy:?}");
            assert_eq!(
                *records.establishment.lock().unwrap(),
                vec![if cancelled {
                    EstablishmentFact::Cancelled
                } else {
                    EstablishmentFact::Failed(true)
                }],
                "{proxy:?}"
            );
            assert!(records.facts.lock().unwrap().is_empty());
            assert_stalled_acquisition(&records, proxy, cancelled);
            server.abort();
            let _ = server.await;
        }
    }
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_proxy_deadline_and_user_cancellation_have_distinct_terminals() {
    for proxy in StalledProxy::cases() {
        for cancelled in [false, true] {
            let (address, started, server) = stalled_proxy().await;
            let records = Arc::new(Records::default());
            let local_records = records.clone();
            tokio::task::spawn_blocking(move || {
                compio_runtime::Runtime::new().unwrap().block_on(async {
                    let builder = aioduct::CompioClient::builder()
                        .observer(TestObserver(local_records))
                        .proxy(proxy.config(address))
                        .connect_timeout(std::time::Duration::from_millis(80));
                    #[cfg(feature = "rustls")]
                    let builder =
                        builder.tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs());
                    let client = builder.build_local().unwrap();
                    let mut send =
                        Box::pin(client.get_local("http://origin.test/").unwrap().send());
                    if cancelled {
                        let mut started = Box::pin(started.notified());
                        match futures_util::future::select(&mut send, &mut started).await {
                            futures_util::future::Either::Right(_) => {}
                            futures_util::future::Either::Left(result) => {
                                panic!("{proxy:?}: unexpected request result {result:?}")
                            }
                        }
                        drop(send);
                    } else {
                        assert!(
                            matches!(send.await.unwrap_err(), aioduct::Error::ConnectTimeout),
                            "{proxy:?}"
                        );
                    }
                });
            })
            .await
            .unwrap();
            assert_eq!(*records.begin_connections.lock().unwrap(), 1, "{proxy:?}");
            assert_eq!(
                *records.establishment.lock().unwrap(),
                vec![if cancelled {
                    EstablishmentFact::Cancelled
                } else {
                    EstablishmentFact::Failed(true)
                }],
                "{proxy:?}"
            );
            assert!(records.facts.lock().unwrap().is_empty());
            assert_stalled_acquisition(&records, proxy, cancelled);
            server.abort();
            let _ = server.await;
        }
    }
}

struct TestObserver(Arc<Records>);
struct TestConnectionObservation(Arc<Records>, usize);

fn forward_request() -> http::Request<Full<Bytes>> {
    http::Request::builder()
        .uri("/observe")
        .header(http::header::HOST, "downstream.test")
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn assert_adaptive_probe_terminal(records: &Records, cancelled: bool) {
    assert_eq!(*records.begin_connections.lock().unwrap(), 1);
    assert_eq!(
        *records.establishment.lock().unwrap(),
        vec![if cancelled {
            EstablishmentFact::Cancelled
        } else {
            EstablishmentFact::Failed(true)
        }]
    );
    assert!(
        records.facts.lock().unwrap().is_empty(),
        "a silent SETTINGS probe is not an established connection"
    );
}

#[tokio::test]
async fn adaptive_h2c_probe_timeout_and_cancellation_do_not_publish_a_connection() {
    for cancelled in [false, true] {
        let (address, started, server) = stalled_proxy().await;
        let records = Arc::new(Records::default());
        let client = aioduct::TokioClient::builder()
            .observer(TestObserver(records.clone()))
            .connect_timeout(std::time::Duration::from_millis(80))
            .build()
            .unwrap();
        let mut send = Box::pin(
            client
                .forward(forward_request())
                .upstream(format!("http://{address}"))
                .adaptive_h2c()
                .connect_timeout(std::time::Duration::from_millis(80))
                .timeout(std::time::Duration::from_secs(2))
                .send(),
        );
        if cancelled {
            tokio::select! {
                result = &mut send => panic!("unexpected result: {result:?}"),
                () = started.notified() => {},
            }
            drop(send);
        } else {
            assert!(matches!(
                send.await.unwrap_err(),
                aioduct::Error::ConnectTimeout
            ));
        }
        assert_adaptive_probe_terminal(&records, cancelled);
        server.abort();
        let _ = server.await;
    }
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_adaptive_probe_terminal_precedes_any_association() {
    for cancelled in [false, true] {
        let (address, started, server) = stalled_proxy().await;
        let records = Arc::new(Records::default());
        let local_records = records.clone();
        tokio::task::spawn_blocking(move || {
            compio_runtime::Runtime::new().unwrap().block_on(async {
                let client = aioduct::CompioClient::builder()
                    .observer(TestObserver(local_records))
                    .connect_timeout(std::time::Duration::from_millis(80))
                    .build_local()
                    .unwrap();
                let mut send = Box::pin(
                    client
                        .forward_local(forward_request())
                        .upstream(format!("http://{address}"))
                        .adaptive_h2c()
                        .connect_timeout(std::time::Duration::from_millis(80))
                        .timeout(std::time::Duration::from_secs(2))
                        .send(),
                );
                if cancelled {
                    let mut started = Box::pin(started.notified());
                    assert!(matches!(
                        futures_util::future::select(&mut send, &mut started).await,
                        futures_util::future::Either::Right(_)
                    ));
                    drop(send);
                } else {
                    assert!(matches!(
                        send.await.unwrap_err(),
                        aioduct::Error::ConnectTimeout
                    ));
                }
            });
        })
        .await
        .unwrap();
        assert_adaptive_probe_terminal(&records, cancelled);
        server.abort();
        let _ = server.await;
    }
}

async fn h2_rejecting_then_h1_origin() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut probe, _) = listener.accept().await.unwrap();
        let mut preface = [0; 24];
        probe.read_exact(&mut preface).await.unwrap();
        assert!(preface.starts_with(b"PRI * HTTP/2.0"));
        probe
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        drop(probe);
        let (mut h1, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let read = h1.read(&mut request).await.unwrap();
        assert!(request[..read].starts_with(b"GET /observe HTTP/1.1"));
        h1.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    (address, server)
}

fn assert_adaptive_fallback(records: &Records) {
    assert_eq!(*records.begin_connections.lock().unwrap(), 2);
    assert_eq!(
        *records.facts.lock().unwrap(),
        vec![Fact::Established(http::Version::HTTP_11)]
    );
    assert_eq!(
        records
            .physical_terminals
            .lock()
            .unwrap()
            .iter()
            .filter(|(owner, _)| *owner == 1)
            .map(|(_, fact)| *fact)
            .collect::<Vec<_>>(),
        vec![EstablishmentFact::Failed(false)]
    );
    assert_eq!(
        records
            .acquisitions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, phase, _)| *phase == ConnectionAcquisitionPhase::TcpConnect)
            .map(|(attempt, _, fact)| (*attempt, *fact))
            .collect::<Vec<_>>(),
        vec![
            (1, AcquisitionFact::Started),
            (1, AcquisitionFact::Completed),
            (1, AcquisitionFact::Started),
            (1, AcquisitionFact::Completed)
        ]
    );
}

#[tokio::test]
async fn rejected_adaptive_candidate_and_h1_fallback_have_separate_physical_owners() {
    let (address, server) = h2_rejecting_then_h1_origin().await;
    let records = Arc::new(Records::default());
    let client = aioduct::TokioClient::builder()
        .observer(TestObserver(records.clone()))
        .build()
        .unwrap();
    let response = client
        .forward(forward_request())
        .upstream(format!("http://{address}"))
        .adaptive_h2c()
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .unwrap();
    assert_eq!(response.bytes().await.unwrap(), "ok");
    assert_adaptive_fallback(&records);
    server.abort();
    let _ = server.await;
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_rejected_adaptive_candidate_has_no_established_or_closed_event() {
    let (address, server) = h2_rejecting_then_h1_origin().await;
    let records = Arc::new(Records::default());
    let local_records = records.clone();
    tokio::task::spawn_blocking(move || {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = aioduct::CompioClient::builder()
                .observer(TestObserver(local_records))
                .build_local()
                .unwrap();
            let response = client
                .forward_local(forward_request())
                .upstream(format!("http://{address}"))
                .adaptive_h2c()
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .unwrap();
            assert_eq!(response.bytes().await.unwrap(), "ok");
        });
    })
    .await
    .unwrap();
    assert_adaptive_fallback(&records);
    server.abort();
    let _ = server.await;
}

#[cfg(any(feature = "rustls", feature = "http3"))]
fn assert_acquisition_terminal(
    records: &Records,
    phase: ConnectionAcquisitionPhase,
    cancelled: bool,
) {
    let phases = records.acquisitions.lock().unwrap();
    assert_eq!(
        phases
            .iter()
            .filter(|(_, p, _)| *p == phase)
            .map(|(attempt, _, fact)| (*attempt, *fact))
            .collect::<Vec<_>>(),
        vec![
            (1, AcquisitionFact::Started),
            (
                1,
                if cancelled {
                    AcquisitionFact::Cancelled
                } else {
                    AcquisitionFact::Failed(true)
                }
            )
        ]
    );
    assert_eq!(
        *records.establishment.lock().unwrap(),
        vec![if cancelled {
            EstablishmentFact::Cancelled
        } else {
            EstablishmentFact::Failed(true)
        }]
    );
    assert!(records.facts.lock().unwrap().is_empty());
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn origin_tls_timeout_and_cancellation_preserve_the_completed_tcp_phase() {
    for cancelled in [false, true] {
        let (address, started, server) = stalled_proxy().await;
        let records = Arc::new(Records::default());
        let client = aioduct::TokioClient::builder()
            .observer(TestObserver(records.clone()))
            .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
            .connect_timeout(std::time::Duration::from_millis(80))
            .build()
            .unwrap();
        let mut send = Box::pin(client.get(&format!("https://{address}/")).unwrap().send());
        if cancelled {
            tokio::select! {
                result = &mut send => panic!("unexpected result {result:?}"),
                () = started.notified() => {},
            }
            drop(send);
        } else {
            assert!(matches!(
                send.await.unwrap_err().into_error(),
                aioduct::Error::ConnectTimeout
            ));
        }
        assert_acquisition_terminal(
            &records,
            ConnectionAcquisitionPhase::TlsHandshake,
            cancelled,
        );
        assert_eq!(
            records
                .acquisitions
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, phase, _)| *phase == ConnectionAcquisitionPhase::TcpConnect)
                .map(|(_, _, fact)| *fact)
                .collect::<Vec<_>>(),
            vec![AcquisitionFact::Started, AcquisitionFact::Completed]
        );
        server.abort();
        let _ = server.await;
    }
}

#[cfg(all(feature = "rustls", feature = "compio"))]
#[tokio::test]
async fn local_origin_tls_timeout_and_cancellation_have_the_actual_phase() {
    for cancelled in [false, true] {
        let (address, started, server) = stalled_proxy().await;
        let records = Arc::new(Records::default());
        let local_records = records.clone();
        tokio::task::spawn_blocking(move || {
            compio_runtime::Runtime::new().unwrap().block_on(async {
                let client = aioduct::CompioClient::builder()
                    .observer(TestObserver(local_records))
                    .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
                    .connect_timeout(std::time::Duration::from_millis(80))
                    .build_local()
                    .unwrap();
                let mut send = Box::pin(
                    client
                        .get_local(&format!("https://{address}/"))
                        .unwrap()
                        .send(),
                );
                if cancelled {
                    let mut started = Box::pin(started.notified());
                    assert!(matches!(
                        futures_util::future::select(&mut send, &mut started).await,
                        futures_util::future::Either::Right(_)
                    ));
                    drop(send);
                } else {
                    assert!(matches!(
                        send.await.unwrap_err(),
                        aioduct::Error::ConnectTimeout
                    ));
                }
            });
        })
        .await
        .unwrap();
        assert_acquisition_terminal(
            &records,
            ConnectionAcquisitionPhase::TlsHandshake,
            cancelled,
        );
        server.abort();
        let _ = server.await;
    }
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn quic_timeout_and_user_cancellation_are_observed_before_http3_setup() {
    for cancelled in [false, true] {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let records = Arc::new(Records::default());
        let client = aioduct::TokioClient::builder()
            .observer(TestObserver(records.clone()))
            .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
            .http3(true)
            .unwrap()
            .connect_timeout(std::time::Duration::from_millis(80))
            .build()
            .unwrap();
        let mut send = Box::pin(client.get(&format!("https://{address}/")).unwrap().send());
        if cancelled {
            let mut packet = [0; 1500];
            tokio::select! {
                result = &mut send => panic!("unexpected result {result:?}"),
                received = socket.recv_from(&mut packet) => { received.unwrap(); },
            }
            drop(send);
        } else {
            assert!(matches!(
                send.await.unwrap_err().into_error(),
                aioduct::Error::ConnectTimeout
            ));
        }
        assert_acquisition_terminal(&records, ConnectionAcquisitionPhase::QuicConnect, cancelled);
    }
}

fn assert_stalled_acquisition(records: &Records, proxy: StalledProxy, cancelled: bool) {
    let phases = records.acquisitions.lock().unwrap();
    // This receiver subscribes only to acquisition, never to attempt callbacks.
    assert!(phases.iter().all(|(attempt, _, _)| *attempt == 1));
    assert_eq!(
        phases
            .iter()
            .filter(|(_, phase, _)| *phase == proxy.stalled_phase())
            .map(|(_, _, fact)| *fact)
            .collect::<Vec<_>>(),
        vec![
            AcquisitionFact::Started,
            if cancelled {
                AcquisitionFact::Cancelled
            } else {
                AcquisitionFact::Failed(true)
            }
        ],
        "{proxy:?}"
    );
    assert_eq!(
        phases
            .iter()
            .filter(|(_, phase, _)| *phase == ConnectionAcquisitionPhase::TcpConnect)
            .map(|(_, _, fact)| *fact)
            .collect::<Vec<_>>(),
        vec![AcquisitionFact::Started, AcquisitionFact::Completed]
    );
}

impl RequestObservation for TestObserver {
    fn on_connection_acquisition(&self, event: &ConnectionAcquisitionEvent<'_>) {
        let fact = match event {
            ConnectionAcquisitionEvent::PhaseStarted { attempt, phase, .. } => {
                (*attempt, *phase, AcquisitionFact::Started)
            }
            ConnectionAcquisitionEvent::PhaseCompleted { attempt, phase, .. } => {
                (*attempt, *phase, AcquisitionFact::Completed)
            }
            ConnectionAcquisitionEvent::Failed {
                attempt,
                phase,
                error,
                ..
            } => (
                *attempt,
                *phase,
                AcquisitionFact::Failed(matches!(error, aioduct::Error::ConnectTimeout)),
            ),
            ConnectionAcquisitionEvent::Cancelled { attempt, phase, .. } => {
                (*attempt, *phase, AcquisitionFact::Cancelled)
            }
            _ => return,
        };
        self.0.acquisitions.lock().unwrap().push(fact);
    }
}

impl Observer for TestObserver {
    type Context = ();
    type Observation = Self;
    type ConnectionObservation = TestConnectionObservation;

    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self::Observation) {
        (Interest::CONNECTION_ACQUISITION, Self(self.0.clone()))
    }

    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        let owner = {
            let mut begins = self.0.begin_connections.lock().unwrap();
            *begins += 1;
            *begins
        };
        (
            Interest::CONNECTION_LIFECYCLE | Interest::CONNECTION_ESTABLISHMENT,
            TestConnectionObservation(self.0.clone(), owner),
        )
    }
}

impl ConnectionObservation for TestConnectionObservation {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        let fact = match event {
            ConnectionObservationEvent::EstablishmentFailed { error, .. } => Some(
                EstablishmentFact::Failed(matches!(error, aioduct::Error::ConnectTimeout)),
            ),
            ConnectionObservationEvent::EstablishmentCancelled { .. } => {
                Some(EstablishmentFact::Cancelled)
            }
            ConnectionObservationEvent::Closed { .. } => Some(EstablishmentFact::Closed),
            _ => None,
        };
        if let Some(fact) = fact {
            self.0.establishment.lock().unwrap().push(fact);
            self.0
                .physical_terminals
                .lock()
                .unwrap()
                .push((self.1, fact));
        }
        if let ConnectionObservationEvent::Established { version, .. } = event {
            self.0
                .facts
                .lock()
                .unwrap()
                .push(Fact::Established(*version));
        }
    }
}

#[derive(Clone, Default)]
struct PausedConnector(Arc<tokio::sync::Notify>);

impl aioduct::runtime::ConnectorSend for PausedConnector {
    type Stream = aioduct::runtime::tokio_rt::TokioIo<tokio::net::TcpStream>;
    async fn connect(&self, _: std::net::SocketAddr) -> std::io::Result<Self::Stream> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[cfg(feature = "compio")]
impl aioduct::runtime::ConnectorLocal for PausedConnector {
    type Stream = aioduct::runtime::compio_rt::CompioTcpStream;
    async fn connect(&self, _: std::net::SocketAddr) -> std::io::Result<Self::Stream> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn establishment_timeout_and_cancellation_are_distinct_terminals() {
    for cancelled in [false, true] {
        let records = Arc::new(Records::default());
        let connector = PausedConnector::default();
        let client =
            aioduct::HttpEngineSend::<TokioRuntime, PausedConnector>::builder_with_connector(
                connector.clone(),
            )
            .observer(TestObserver(records.clone()))
            .connect_timeout(std::time::Duration::from_millis(60))
            .build()
            .unwrap();
        let mut send = Box::pin(client.get("http://127.0.0.1:9/").unwrap().send());
        if cancelled {
            tokio::select! {
                result = &mut send => panic!("unexpected result: {result:?}"),
                () = connector.0.notified() => {},
            }
            drop(send);
        } else {
            assert!(matches!(
                send.await.unwrap_err().into_error(),
                aioduct::Error::ConnectTimeout
            ));
        }
        assert_eq!(
            *records.establishment.lock().unwrap(),
            vec![if cancelled {
                EstablishmentFact::Cancelled
            } else {
                EstablishmentFact::Failed(true)
            }]
        );
        assert!(records.facts.lock().unwrap().is_empty());
    }
}

#[cfg(feature = "compio")]
#[tokio::test]
async fn local_establishment_timeout_and_cancellation_are_distinct_terminals() {
    for cancelled in [false, true] {
        let records = Arc::new(Records::default());
        let local_records = records.clone();
        tokio::task::spawn_blocking(move || {
            compio_runtime::Runtime::new().unwrap().block_on(async {
                let connector = PausedConnector::default();
                let client = aioduct::HttpEngineLocal::<
                    aioduct::runtime::compio_rt::CompioRuntime,
                    PausedConnector,
                >::builder_with_connector(connector.clone())
                .observer(TestObserver(local_records))
                .connect_timeout(std::time::Duration::from_millis(60))
                .build_local()
                .unwrap();
                let mut send = Box::pin(client.get_local("http://127.0.0.1:9/").unwrap().send());
                if cancelled {
                    let mut started = Box::pin(connector.0.notified());
                    match futures_util::future::select(&mut send, &mut started).await {
                        futures_util::future::Either::Right(_) => {}
                        futures_util::future::Either::Left(_) => panic!("unexpected request end"),
                    }
                    drop(send);
                } else {
                    assert!(matches!(
                        send.await.unwrap_err(),
                        aioduct::Error::ConnectTimeout
                    ));
                }
            });
        })
        .await
        .unwrap();
        assert_eq!(
            *records.establishment.lock().unwrap(),
            vec![if cancelled {
                EstablishmentFact::Cancelled
            } else {
                EstablishmentFact::Failed(true)
            }]
        );
        assert!(records.facts.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn fresh_connection_creates_one_owner_and_reuse_keeps_one_association() {
    let address = aioduct_test_server::h1::spawn_h1_server_with(move |_| async move {
        Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(b"ok"))))
    });
    let records = Arc::new(Records::default());
    let client = aioduct::HttpEngineSend::<TokioRuntime, TcpConnector, _>::builder()
        .observer(TestObserver(records.clone()))
        .build()
        .unwrap();

    let first = client.get(&format!("http://{address}/first")).unwrap();
    assert_eq!(
        first.send().await.unwrap().bytes().await.unwrap(),
        Bytes::from_static(b"ok")
    );
    let second = client.get(&format!("http://{address}/second")).unwrap();
    assert_eq!(
        second.send().await.unwrap().bytes().await.unwrap(),
        Bytes::from_static(b"ok")
    );

    assert_eq!(*records.begin_connections.lock().unwrap(), 1);
    assert_eq!(
        records.facts.lock().unwrap().as_slice(),
        &[Fact::Established(http::Version::HTTP_11)]
    );
}

struct EstablishmentOnly(Arc<std::sync::atomic::AtomicUsize>);
struct DropProbe(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl ConnectionObservation for DropProbe {}
impl Observer for EstablishmentOnly {
    type Context = ();
    type Observation = NoopRequestObservation;
    type ConnectionObservation = DropProbe;
    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self::Observation) {
        (Interest::EMPTY, NoopRequestObservation)
    }
    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (
            Interest::CONNECTION_ESTABLISHMENT,
            DropProbe(self.0.clone()),
        )
    }
}

#[tokio::test]
async fn establishment_only_state_is_released_before_returning_pooled_response() {
    let address = aioduct_test_server::h1::spawn_h1_server_with(move |_| async move {
        Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(b"ok"))))
    });
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let client = aioduct::TokioClient::builder()
        .observer(EstablishmentOnly(drops.clone()))
        .build()
        .unwrap();
    let response = client
        .get(&format!("http://{address}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
    response.bytes().await.unwrap();
    client
        .get(&format!("http://{address}/reuse"))
        .unwrap()
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[cfg(feature = "compio")]
#[test]
fn local_establishment_only_state_is_not_retained_in_the_registry() {
    let server = tokio::runtime::Runtime::new().unwrap();
    let address = {
        let _entered = server.enter();
        aioduct_test_server::h1::spawn_h1_server_with(move |_| async move {
            Ok::<_, Infallible>(http::Response::new(Full::new(Bytes::from_static(b"ok"))))
        })
    };
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = aioduct::CompioClient::builder()
            .observer(EstablishmentOnly(drops.clone()))
            .build_local()
            .unwrap();
        let response = client
            .get(&format!("http://{address}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
        response.bytes().await.unwrap();
    });
}
