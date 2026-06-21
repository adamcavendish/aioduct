pub(crate) use std::convert::Infallible;
pub(crate) use std::future::Future;
pub(crate) use std::io;
pub(crate) use std::net::{IpAddr, SocketAddr};
pub(crate) use std::sync::Arc;
pub(crate) use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicUsize, Ordering as AtomicOrdering,
};
pub(crate) use std::time::Duration;

pub(crate) use aioduct::HttpEngineSend;
pub(crate) use aioduct::runtime::TokioRuntime;
pub(crate) use aioduct::runtime::tokio_rt::TcpConnector;
pub(crate) use aioduct::runtime::{ConnectorSend, SocketConfig};
pub(crate) use aioduct_test_server::h1::{h1_server, h1_server_with};
pub(crate) use aioduct_test_server::raw::raw_server;
pub(crate) use bytes::Bytes;
pub(crate) use http_body_util::Full;
pub(crate) use hyper::Response;
pub(crate) use tokio::net::TcpListener;

const EXPECTED_PROXY_AUTH: &str = "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==";
pub(crate) type CapturedConnects = Arc<std::sync::Mutex<Vec<String>>>;

pub(crate) fn captured_connects() -> CapturedConnects {
    Arc::new(std::sync::Mutex::new(Vec::new()))
}

pub(crate) fn connect_target(connect_req: &str) -> &str {
    connect_req.split_whitespace().nth(1).unwrap_or("")
}

fn proxy_auth_value(connect_req: &str) -> Option<&str> {
    connect_req
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("proxy-authorization"))
        .map(|(_, value)| value.trim())
}

pub(crate) fn assert_connect_for_target_has_auth(
    captured_connects: &CapturedConnects,
    target: &str,
) {
    let connect_reqs = captured_connects.lock().unwrap();
    let connect_req = connect_reqs
        .iter()
        .find(|req| req.starts_with("CONNECT ") && connect_target(req) == target)
        .unwrap_or_else(|| panic!("expected CONNECT request for {target}, got: {connect_reqs:?}"));

    assert_eq!(proxy_auth_value(connect_req), Some(EXPECTED_PROXY_AUTH));
}

#[derive(Clone)]
pub(crate) struct ProxyKeepaliveCountingConnector {
    inner: TcpConnector,
    keepalive_calls: Arc<AtomicU32>,
}

impl ProxyKeepaliveCountingConnector {
    pub(crate) fn new() -> Self {
        Self {
            inner: TcpConnector,
            keepalive_calls: Arc::new(AtomicU32::new(0)),
        }
    }

    pub(crate) fn keepalive_calls(&self) -> u32 {
        self.keepalive_calls.load(AtomicOrdering::SeqCst)
    }
}

pub(crate) struct ProxyKeepaliveCountingStream {
    inner: <TcpConnector as ConnectorSend>::Stream,
    counter: Arc<AtomicU32>,
}

impl SocketConfig for ProxyKeepaliveCountingStream {
    fn set_keepalive(
        &self,
        time: Duration,
        interval: Option<Duration>,
        retries: Option<u32>,
    ) -> io::Result<()> {
        self.counter.fetch_add(1, AtomicOrdering::SeqCst);
        self.inner.set_keepalive(time, interval, retries)
    }

    fn set_fast_open(&self) -> io::Result<()> {
        self.inner.set_fast_open()
    }
}

impl hyper::rt::Read for ProxyKeepaliveCountingStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for ProxyKeepaliveCountingStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl Unpin for ProxyKeepaliveCountingStream {}

impl ConnectorSend for ProxyKeepaliveCountingConnector {
    type Stream = ProxyKeepaliveCountingStream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let counter = Arc::clone(&self.keepalive_calls);
        async move {
            let stream = inner.connect(addr).await?;
            Ok(ProxyKeepaliveCountingStream {
                inner: stream,
                counter,
            })
        }
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let inner = self.inner;
        let counter = Arc::clone(&self.keepalive_calls);
        async move {
            let stream = inner.connect_bound(addr, local).await?;
            Ok(ProxyKeepaliveCountingStream {
                inner: stream,
                counter,
            })
        }
    }
}

/// A real CONNECT proxy: accepts CONNECT, responds 200, then relays bytes
/// bidirectionally between client and target. Parses the CONNECT target
/// (`host:port`) to connect to the correct server. Each accepted TCP connection
/// counts as one proxy connection.
pub(crate) async fn connect_proxy() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    connect_proxy_with_capture(None).await
}

pub(crate) async fn connect_proxy_with_capture(
    captured_connects: Option<CapturedConnects>,
) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conn_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&conn_count);

    tokio::spawn(async move {
        loop {
            let (mut client, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            cc.fetch_add(1, AtomicOrdering::SeqCst);
            let captured_connects = captured_connects.clone();

            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 512];
                loop {
                    let n = match client.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if buf.len() > 8192 {
                        return;
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                if !head.starts_with("CONNECT ") {
                    return;
                }
                if let Some(captured_connects) = captured_connects {
                    captured_connects.lock().unwrap().push(head.clone());
                }
                let target = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                let mut target = TcpStream::connect(&target).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut target).await;
            });
        }
    });

    (addr, conn_count)
}

/// A TLS CONNECT proxy: the client establishes a TLS connection TO THE PROXY
/// (https:// proxy scheme) before any CONNECT request.
#[cfg(feature = "rustls")]
pub(crate) async fn tls_connect_proxy() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicUsize>,
) {
    tls_connect_proxy_with_capture(None).await
}

#[cfg(feature = "rustls")]
pub(crate) async fn tls_connect_proxy_with_capture(
    captured_connects: Option<CapturedConnects>,
) -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicUsize>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    aioduct_test_server::tls::install_crypto_provider();

    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();

    let server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
            .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("localhost:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conn_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&conn_count);

    tokio::spawn(async move {
        loop {
            let (tcp, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => return,
            };
            cc.fetch_add(1, AtomicOrdering::SeqCst);
            let acceptor = acceptor.clone();
            let captured_connects = captured_connects.clone();
            tokio::spawn(async move {
                let mut client = match acceptor.accept(tcp).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 512];
                loop {
                    let n = match client.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if buf.len() > 8192 {
                        return;
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                if !head.starts_with("CONNECT ") {
                    return;
                }
                if let Some(captured_connects) = captured_connects {
                    captured_connects.lock().unwrap().push(head.clone());
                }
                let target = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                let mut upstream = match TcpStream::connect(&target).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    (addr, cert_der, conn_count)
}

/// Build a client TLS config that trusts multiple self-signed certs.
#[cfg(feature = "rustls")]
pub(crate) fn client_config_trusting(
    certs: &[rustls::pki_types::CertificateDer<'static>],
) -> std::sync::Arc<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    for cert in certs {
        root_store.add(cert.clone()).unwrap();
    }
    let mut config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_root_certificates(root_store)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    std::sync::Arc::new(config)
}
