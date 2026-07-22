use std::future::{Future, pending};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aioduct::runtime::ConnectorSend;
use aioduct::runtime::smol_rt::{SmolIo, SmolRuntime, TcpConnector};
use aioduct::{Error, HttpEngineSend, ProxyChain, ProxyConfig, Resolve};
use smol::io::{AsyncReadExt, AsyncWriteExt};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(80);
const TEST_TIMEOUT: Duration = Duration::from_secs(2);

fn assert_connect_timeout(error: &aioduct::SendError) {
    assert!(
        matches!(error.error(), Error::ConnectTimeout),
        "expected ConnectTimeout, got: {error:?}"
    );
}

#[derive(Clone, Copy)]
struct PendingResolver;

impl Resolve for PendingResolver {
    fn resolve(
        &self,
        _host: &str,
        _port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        Box::pin(pending())
    }
}

#[derive(Clone, Default)]
struct PendingConnector {
    attempts: Arc<AtomicUsize>,
}

impl PendingConnector {
    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

impl ConnectorSend for PendingConnector {
    type Stream = SmolIo<smol::net::TcpStream>;

    fn connect(&self, _addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let attempts = Arc::clone(&self.attempts);
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            pending().await
        }
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

async fn await_task(task: smol::Task<()>, description: &'static str) {
    smol::future::race(task, async move {
        async_io::Timer::after(TEST_TIMEOUT).await;
        panic!("timed out waiting for {description}");
    })
    .await;
}

async fn start_stalled_server() -> (SocketAddr, Arc<Mutex<Vec<u8>>>, smol::Task<()>) {
    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let server_received = Arc::clone(&received);
    let task = smol::spawn(async move {
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
    (addr, received, task)
}

async fn read_headers(stream: &mut smol::net::TcpStream) -> io::Result<Vec<u8>> {
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

async fn start_delayed_chain_proxy(
    delay: Duration,
) -> (SocketAddr, SocketAddr, Arc<AtomicUsize>, smol::Task<()>) {
    let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_addr = listener.local_addr().unwrap();
    let second_addr = SocketAddr::from(([127, 0, 0, 1], 65_534));
    let negotiations = Arc::new(AtomicUsize::new(0));
    let server_negotiations = Arc::clone(&negotiations);
    let task = smol::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        let first = read_headers(&mut stream).await.unwrap();
        assert!(
            first.starts_with(format!("CONNECT {second_addr} HTTP/1.1\r\n").as_bytes()),
            "unexpected first-hop request: {}",
            String::from_utf8_lossy(&first)
        );
        server_negotiations.fetch_add(1, Ordering::SeqCst);
        async_io::Timer::after(delay).await;
        if stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            return;
        }

        let second = read_headers(&mut stream).await.unwrap();
        assert!(
            second.starts_with(b"CONNECT origin.test:80 HTTP/1.1\r\n"),
            "unexpected second-hop request: {}",
            String::from_utf8_lossy(&second)
        );
        server_negotiations.fetch_add(1, Ordering::SeqCst);
        async_io::Timer::after(delay).await;
        if stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            return;
        }

        if read_headers(&mut stream).await.is_ok() {
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await;
        }
    });
    (first_addr, second_addr, negotiations, task)
}

#[test]
fn dns_resolution_uses_the_connection_deadline() {
    smol::block_on(async {
        let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder()
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
    });
}

#[test]
fn tcp_connect_uses_the_connection_deadline() {
    smol::block_on(async {
        let connector = PendingConnector::default();
        let client = HttpEngineSend::<SmolRuntime, PendingConnector>::builder_with_connector(
            connector.clone(),
        )
        .connect_timeout(CONNECT_TIMEOUT)
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
    });
}

#[test]
fn pool_coordination_uses_the_connection_deadline() {
    smol::block_on(async {
        let connector = PendingConnector::default();
        let client = HttpEngineSend::<SmolRuntime, PendingConnector>::builder_with_connector(
            connector.clone(),
        )
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
        let url = "http://127.0.0.1:9/";

        let first_client = client.clone();
        let first = smol::spawn(async move {
            first_client
                .get(url)
                .unwrap()
                .h2c_prior_knowledge()
                .connect_timeout(Duration::from_millis(400))
                .send()
                .await
        });
        wait_for_connection_attempt(&connector).await;

        let error = client
            .get(url)
            .unwrap()
            .h2c_prior_knowledge()
            .connect_timeout(CONNECT_TIMEOUT)
            .send()
            .await
            .unwrap_err();

        assert_connect_timeout(&error);
        assert_eq!(
            connector.attempts(),
            1,
            "pool waiter must not start a second connection"
        );
        assert_connect_timeout(&first.await.unwrap_err());
    });
}

#[cfg(feature = "rustls")]
#[test]
fn origin_tls_handshake_uses_the_connection_deadline() {
    smol::block_on(async {
        let (addr, received, server) = start_stalled_server().await;
        let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder()
            .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        let error = client
            .get(&format!("https://127.0.0.1:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert_connect_timeout(&error);
        await_task(server, "stalled origin TLS server").await;
        assert_eq!(
            received.lock().unwrap().first(),
            Some(&0x16),
            "origin did not receive a TLS handshake record"
        );
    });
}

#[test]
fn connect_response_uses_the_connection_deadline() {
    smol::block_on(async {
        let (proxy_addr, received, server) = start_stalled_server().await;
        let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder()
            .proxy(ProxyConfig::http(&format!("http://{proxy_addr}")).unwrap())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        let error = client
            .get("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert_connect_timeout(&error);
        await_task(server, "stalled CONNECT server").await;
        assert!(
            received
                .lock()
                .unwrap()
                .starts_with(b"CONNECT origin.test:80 HTTP/1.1\r\n"),
            "proxy did not receive the CONNECT request"
        );
    });
}

#[test]
fn socks4_negotiation_uses_the_connection_deadline() {
    smol::block_on(async {
        let (proxy_addr, received, server) = start_stalled_server().await;
        let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder()
            .proxy(ProxyConfig::socks4(&format!("socks4a://{proxy_addr}")).unwrap())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        let error = client
            .get("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert_connect_timeout(&error);
        await_task(server, "stalled SOCKS4 server").await;
        assert_eq!(
            received.lock().unwrap().first(),
            Some(&0x04),
            "proxy did not receive a SOCKS4 negotiation"
        );
    });
}

#[test]
fn socks5_negotiation_uses_the_connection_deadline() {
    smol::block_on(async {
        let (proxy_addr, received, server) = start_stalled_server().await;
        let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder()
            .proxy(ProxyConfig::socks5h(&format!("socks5h://{proxy_addr}")).unwrap())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        let error = client
            .get("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert_connect_timeout(&error);
        await_task(server, "stalled SOCKS5 server").await;
        assert_eq!(
            received.lock().unwrap().first(),
            Some(&0x05),
            "proxy did not receive a SOCKS5 negotiation"
        );
    });
}

#[test]
fn chained_proxy_hops_share_one_connection_budget() {
    smol::block_on(async {
        let delay = Duration::from_millis(200);
        let (first_addr, second_addr, negotiations, server) =
            start_delayed_chain_proxy(delay).await;
        let chain = ProxyChain::new(vec![
            ProxyConfig::http(&format!("http://{first_addr}")).unwrap(),
            ProxyConfig::http(&format!("http://{second_addr}")).unwrap(),
        ]);
        let client = HttpEngineSend::<SmolRuntime, TcpConnector>::builder()
            .proxy_chain(chain)
            .connect_timeout(Duration::from_millis(300))
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        let error = client
            .get("http://origin.test/")
            .unwrap()
            .send()
            .await
            .unwrap_err();

        assert_connect_timeout(&error);
        assert_eq!(
            negotiations.load(Ordering::SeqCst),
            2,
            "the shared budget expired before the second proxy negotiation began"
        );
        await_task(server, "delayed proxy-chain server").await;
    });
}
