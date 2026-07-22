use std::future::{Future, pending};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aioduct::runtime::ConnectorLocal;
use aioduct::runtime::compio_rt::CompioTcpStream;
use aioduct::{Error, ProxyChain, ProxyConfig, Resolve};

use super::*;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(80);
const TEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct PendingResolver;

#[derive(Clone, Default)]
struct PendingConnector {
    attempts: Arc<AtomicUsize>,
}

impl PendingConnector {
    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

impl ConnectorLocal for PendingConnector {
    type Stream = CompioTcpStream;

    async fn connect(&self, _addr: SocketAddr) -> io::Result<Self::Stream> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        pending().await
    }
}

#[derive(Clone, Default)]
struct DeadlineProxyObserver(std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>);

impl aioduct::observer::RequestObserver for DeadlineProxyObserver {
    fn on_event(&self, event: &aioduct::observer::RequestEvent) {
        if matches!(
            event.phase,
            aioduct::observer::RequestPhase::TcpConnected { .. }
        ) {
            self.0.lock().unwrap().push("tcp");
        }
    }

    fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
}

impl Resolve for PendingResolver {
    fn resolve(
        &self,
        _host: &str,
        _port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        Box::pin(pending())
    }
}

async fn wait_for_connection_attempt(connector: &PendingConnector) {
    for _ in 0..200 {
        if connector.attempts() != 0 {
            return;
        }
        async_io::Timer::after(Duration::from_millis(1)).await;
    }
    panic!("connection attempt did not start");
}

fn join_server(server: std::thread::JoinHandle<()>, description: &str) {
    let deadline = std::time::Instant::now() + TEST_TIMEOUT;
    while !server.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(server.is_finished(), "timed out waiting for {description}");
    server.join().unwrap();
}

async fn read_headers_tokio(stream: &mut tokio::net::TcpStream) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    let mut headers = Vec::with_capacity(512);
    let mut buf = [0u8; 256];
    while headers.len() < 16 * 1024 {
        let read = stream.read(&mut buf).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before headers completed",
            ));
        }
        headers.extend_from_slice(&buf[..read]);
        if headers.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(headers);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "headers exceeded test limit",
    ))
}

fn start_stalled_server_tokio() -> (SocketAddr, Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let received = Arc::new(Mutex::new(Vec::new()));
    let server_received = Arc::clone(&received);
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 256];
            loop {
                let read = stream.read(&mut bytes).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                server_received
                    .lock()
                    .unwrap()
                    .extend_from_slice(&bytes[..read]);
            }
        });
    });
    (rx.recv().unwrap(), received, server)
}

fn start_delayed_chain_proxy_tokio(
    delay: Duration,
) -> (
    SocketAddr,
    SocketAddr,
    Arc<AtomicUsize>,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let negotiations = Arc::new(AtomicUsize::new(0));
    let server_negotiations = Arc::clone(&negotiations);
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            use tokio::io::AsyncWriteExt;

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let first_addr = listener.local_addr().unwrap();
            let second_addr = SocketAddr::from(([127, 0, 0, 1], 65_534));
            tx.send((first_addr, second_addr)).unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();

            let first = read_headers_tokio(&mut stream).await.unwrap();
            assert!(
                first.starts_with(format!("CONNECT {second_addr} HTTP/1.1\r\n").as_bytes()),
                "unexpected first-hop request: {}",
                String::from_utf8_lossy(&first)
            );
            server_negotiations.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            if stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }

            let second = read_headers_tokio(&mut stream).await.unwrap();
            assert!(
                second.starts_with(b"CONNECT origin.test:80 HTTP/1.1\r\n"),
                "unexpected second-hop request: {}",
                String::from_utf8_lossy(&second)
            );
            server_negotiations.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            if stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }

            if read_headers_tokio(&mut stream).await.is_ok() {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
            }
        });
    });
    let (first_addr, second_addr) = rx.recv().unwrap();
    (first_addr, second_addr, negotiations, server)
}

#[test]
fn local_dns_resolution_uses_the_connection_deadline() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolver(PendingResolver)
            .connect_timeout(Duration::from_millis(60))
            .timeout(Duration::from_secs(1))
            .build_local()
            .unwrap();

        let error = client
            .get_local("http://pending.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
    });
}

#[test]
fn local_tcp_connect_uses_the_connection_deadline() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = PendingConnector::default();
        let client = HttpEngineLocal::<CompioRuntime, PendingConnector>::builder_with_connector(
            connector.clone(),
        )
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(1))
        .build_local()
        .unwrap();

        let error = client
            .get_local("http://127.0.0.1:9/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
        assert_eq!(connector.attempts(), 1);
    });
}

#[test]
fn local_pool_coordination_uses_the_connection_deadline() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector = PendingConnector::default();
        let client = HttpEngineLocal::<CompioRuntime, PendingConnector>::builder_with_connector(
            connector.clone(),
        )
        .timeout(Duration::from_secs(1))
        .build_local()
        .unwrap();
        let url = "http://127.0.0.1:9/";

        let first_client = client.clone();
        let first = compio_runtime::spawn(async move {
            first_client
                .get_local(url)
                .unwrap()
                .h2c_prior_knowledge()
                .connect_timeout(Duration::from_millis(400))
                .send()
                .await
        });
        wait_for_connection_attempt(&connector).await;

        let error = client
            .get_local(url)
            .unwrap()
            .h2c_prior_knowledge()
            .connect_timeout(CONNECT_TIMEOUT)
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
        assert_eq!(
            connector.attempts(),
            1,
            "pool waiter must not start a second connection"
        );
        let first_error = first.await.unwrap().unwrap_err();
        assert!(
            matches!(first_error, Error::ConnectTimeout),
            "expected first request to time out, got: {first_error:?}"
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn local_origin_tls_handshake_uses_the_connection_deadline() {
    let (addr, received, server) = start_stalled_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(1))
            .build_local()
            .unwrap();

        let error = client
            .get_local(&format!("https://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
    });
    join_server(server, "stalled origin TLS server");
    assert_eq!(
        received.lock().unwrap().first(),
        Some(&0x16),
        "origin did not receive a TLS handshake record"
    );
}

#[test]
fn local_connect_response_uses_the_connection_deadline() {
    let (proxy_addr, received, server) = start_stalled_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let observer = DeadlineProxyObserver::default();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .request_observer(observer.clone())
            .connect_timeout(Duration::from_millis(80))
            .timeout(Duration::from_secs(1))
            .build_local()
            .unwrap();

        let error = client
            .get_local("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
        assert_eq!(
            observer.0.lock().unwrap().as_slice(),
            &["tcp"],
            "completed proxy TCP phase was lost on cancellation"
        );
    });
    join_server(server, "stalled CONNECT server");
    assert!(
        received
            .lock()
            .unwrap()
            .starts_with(b"CONNECT origin.test:80 HTTP/1.1\r\n"),
        "proxy did not receive the CONNECT request"
    );
}

#[test]
fn local_socks4_negotiation_uses_the_connection_deadline() {
    let (proxy_addr, received, server) = start_stalled_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(ProxyConfig::socks4(&format!("socks4a://{proxy_addr}")).unwrap())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(1))
            .build_local()
            .unwrap();

        let error = client
            .get_local("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
    });
    join_server(server, "stalled SOCKS4 server");
    assert_eq!(
        received.lock().unwrap().first(),
        Some(&0x04),
        "proxy did not receive a SOCKS4 negotiation"
    );
}

#[test]
fn local_socks5_negotiation_uses_the_connection_deadline() {
    let (proxy_addr, received, server) = start_stalled_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy(ProxyConfig::socks5h(&format!("socks5h://{proxy_addr}")).unwrap())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(1))
            .build_local()
            .unwrap();

        let error = client
            .get_local("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
    });
    join_server(server, "stalled SOCKS5 server");
    assert_eq!(
        received.lock().unwrap().first(),
        Some(&0x05),
        "proxy did not receive a SOCKS5 negotiation"
    );
}

#[test]
fn local_chained_proxy_hops_share_one_connection_budget() {
    let delay = Duration::from_millis(200);
    let (first_addr, second_addr, negotiations, server) = start_delayed_chain_proxy_tokio(delay);
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let chain = ProxyChain::new(vec![
            ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
            ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
        ]);
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .proxy_chain(chain)
            .connect_timeout(Duration::from_millis(300))
            .timeout(Duration::from_secs(1))
            .build_local()
            .unwrap();

        let error = client
            .get_local("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, Error::ConnectTimeout),
            "expected ConnectTimeout, got: {error:?}"
        );
        assert_eq!(
            negotiations.load(Ordering::SeqCst),
            2,
            "the shared budget expired before the second proxy negotiation began"
        );
    });
    join_server(server, "delayed proxy-chain server");
}
