use std::future::{Future, pending};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aioduct::runtime::tokio_rt::{TcpConnector, TokioIo};
use aioduct::runtime::{ConnectorSend, TokioRuntime};
use aioduct::{Error, HttpEngineSend, ProxyChain, ProxyConfig, Resolve};
use bytes::Bytes;
use http_body_util::Full;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

#[derive(Clone, Copy)]
struct DelayedResolver {
    addr: SocketAddr,
    delay: Duration,
}

impl Resolve for DelayedResolver {
    fn resolve(
        &self,
        _host: &str,
        _port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<SocketAddr>> + Send>> {
        let addr = self.addr;
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(addr)
        })
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
    type Stream = TokioIo<TcpStream>;

    fn connect(&self, _addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let attempts = Arc::clone(&self.attempts);
        async move {
            attempts.fetch_add(1, Ordering::SeqCst);
            pending().await
        }
    }
}

#[derive(Clone, Copy)]
struct DelayedConnector {
    inner: TcpConnector,
    delay: Duration,
}

#[derive(Clone)]
struct DelayAfterFirstConnector {
    inner: TcpConnector,
    attempts: Arc<AtomicUsize>,
    delay: Duration,
}

impl ConnectorSend for DelayAfterFirstConnector {
    type Stream = <TcpConnector as ConnectorSend>::Stream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        let delay = self.delay;
        async move {
            if attempt > 0 {
                tokio::time::sleep(delay).await;
            }
            inner.connect(addr).await
        }
    }
}

impl ConnectorSend for DelayedConnector {
    type Stream = <TcpConnector as ConnectorSend>::Stream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let delay = self.delay;
        async move {
            tokio::time::sleep(delay).await;
            inner.connect(addr).await
        }
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let delay = self.delay;
        async move {
            tokio::time::sleep(delay).await;
            inner.connect_bound(addr, local).await
        }
    }
}

async fn start_stalled_tcp_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut byte = [0u8; 1];
        let _ = stream.read(&mut byte).await;
        pending::<()>().await;
    });
    (addr, task)
}

#[cfg(feature = "rustls")]
async fn start_connect_then_stall() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        if read_headers(&mut stream).await.is_err() {
            return;
        }
        if stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            return;
        }
        let mut byte = [0u8; 1];
        let _ = stream.read(&mut byte).await;
        pending::<()>().await;
    });
    (addr, task)
}

#[cfg(feature = "rustls")]
async fn start_tls_connect_then_stall() -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    tokio::task::JoinHandle<()>,
) {
    aioduct_test_server::tls::install_crypto_provider();
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider supports default protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("localhost:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = acceptor.accept(stream).await.unwrap();
        read_headers(&mut stream).await.unwrap();
        pending::<()>().await;
    });
    (addr, cert_der, task)
}

async fn read_headers<S>(stream: &mut S) -> io::Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + Unpin,
{
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

async fn start_delayed_two_hop_chain(
    delay: Duration,
) -> (
    SocketAddr,
    SocketAddr,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
) {
    let second_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_addr = second_listener.local_addr().unwrap();
    let second_task = tokio::spawn(async move {
        let Ok((mut client, _)) = second_listener.accept().await else {
            return;
        };
        if read_headers(&mut client).await.is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
        if client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            return;
        }
        if read_headers(&mut client).await.is_err() {
            return;
        }
        let _ = client
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await;
    });

    let first_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_addr = first_listener.local_addr().unwrap();
    let first_task = tokio::spawn(async move {
        let Ok((mut client, _)) = first_listener.accept().await else {
            return;
        };
        if read_headers(&mut client).await.is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
        let Ok(mut second) = TcpStream::connect(second_addr).await else {
            return;
        };
        if client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
        {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut second).await;
    });

    (first_addr, second_addr, first_task, second_task)
}

async fn start_delayed_stale_h1_server(
    stale_delay: Duration,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stale, _) = listener.accept().await.unwrap();
        read_headers(&mut stale).await.unwrap();
        stale
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
        read_headers(&mut stale).await.unwrap();
        tokio::time::sleep(stale_delay).await;
        drop(stale);

        let (mut fresh, _) = listener.accept().await.unwrap();
        read_headers(&mut fresh).await.unwrap();
        fresh
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nfresh")
            .await
            .unwrap();
    });
    (addr, task)
}

#[path = "connection_acquisition/coordination.rs"]
mod coordination;
#[path = "connection_acquisition/direct.rs"]
mod direct;
#[path = "connection_acquisition/proxy.rs"]
mod proxy;
