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

/// Accepts TCP connections, consumes one byte from each, and closes before
/// completing any proxy transport or negotiation.
pub(crate) async fn closing_proxy_endpoint() -> (SocketAddr, Arc<AtomicUsize>) {
    use tokio::io::AsyncReadExt;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = Arc::clone(&connections);
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            server_connections.fetch_add(1, AtomicOrdering::SeqCst);
            tokio::spawn(async move {
                let mut byte = [0_u8; 1];
                let _ = stream.read(&mut byte).await;
            });
        }
    });

    (addr, connections)
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

#[cfg(feature = "rustls")]
pub(crate) async fn socks5_proxy() -> std::net::SocketAddr {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut greeting = [0u8; 2];
                if client.read_exact(&mut greeting).await.is_err() || greeting[0] != 5 {
                    return;
                }
                let mut methods = vec![0u8; greeting[1] as usize];
                if client.read_exact(&mut methods).await.is_err() {
                    return;
                }
                if client.write_all(&[5, 0]).await.is_err() {
                    return;
                }

                let mut request = [0u8; 4];
                if client.read_exact(&mut request).await.is_err() || request[..3] != [5, 1, 0] {
                    return;
                }
                let host = match request[3] {
                    1 => {
                        let mut octets = [0u8; 4];
                        if client.read_exact(&mut octets).await.is_err() {
                            return;
                        }
                        Ipv4Addr::from(octets).to_string()
                    }
                    3 => {
                        let Ok(length) = client.read_u8().await else {
                            return;
                        };
                        let mut host = vec![0u8; length as usize];
                        if client.read_exact(&mut host).await.is_err() {
                            return;
                        }
                        String::from_utf8_lossy(&host).into_owned()
                    }
                    4 => {
                        let mut octets = [0u8; 16];
                        if client.read_exact(&mut octets).await.is_err() {
                            return;
                        }
                        Ipv6Addr::from(octets).to_string()
                    }
                    _ => return,
                };
                let Ok(port) = client.read_u16().await else {
                    return;
                };
                let Ok(mut upstream) = tokio::net::TcpStream::connect((host.as_str(), port)).await
                else {
                    return;
                };
                if client
                    .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
                    .is_err()
                {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
}

#[cfg(feature = "rustls")]
pub(crate) async fn socks4_proxy() -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_nul_terminated(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
        use tokio::io::AsyncReadExt;

        let mut value = Vec::new();
        while value.len() <= 1024 {
            let byte = stream.read_u8().await.ok()?;
            if byte == 0 {
                return Some(value);
            }
            value.push(byte);
        }
        None
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut request = [0u8; 8];
                if client.read_exact(&mut request).await.is_err()
                    || request[0] != 4
                    || request[1] != 1
                {
                    return;
                }
                let port = u16::from_be_bytes([request[2], request[3]]);
                if read_nul_terminated(&mut client).await.is_none() {
                    return;
                }
                let host = if request[4..8] == [0, 0, 0, 1] {
                    let Some(host) = read_nul_terminated(&mut client).await else {
                        return;
                    };
                    String::from_utf8_lossy(&host).into_owned()
                } else {
                    std::net::Ipv4Addr::new(request[4], request[5], request[6], request[7])
                        .to_string()
                };
                let Ok(mut upstream) = tokio::net::TcpStream::connect((host.as_str(), port)).await
                else {
                    let _ = client.write_all(&[0, 0x5b, 0, 0, 0, 0, 0, 0]).await;
                    return;
                };
                if client
                    .write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0])
                    .await
                    .is_err()
                {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
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
    tls_connect_proxy_with_capture_and_alpn(
        captured_connects,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        None,
    )
    .await
}

#[cfg(feature = "rustls")]
pub(crate) async fn tls_connect_proxy_without_alpn() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicUsize>,
) {
    tls_connect_proxy_with_capture_and_alpn(None, Vec::new(), None).await
}

#[cfg(feature = "rustls")]
pub(crate) async fn tls_connect_proxy_observing_client_certificate(
    client_certificate: rustls::pki_types::CertificateDer<'static>,
) -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicBool>,
) {
    let client_certificate_seen = Arc::new(AtomicBool::new(false));
    let (addr, proxy_certificate, _) = tls_connect_proxy_with_capture_and_alpn(
        None,
        vec![b"http/1.1".to_vec()],
        Some((client_certificate, client_certificate_seen.clone())),
    )
    .await;
    (addr, proxy_certificate, client_certificate_seen)
}

#[cfg(feature = "rustls")]
async fn tls_connect_proxy_with_capture_and_alpn(
    captured_connects: Option<CapturedConnects>,
    alpn_protocols: Vec<Vec<u8>>,
    client_auth_observation: Option<(rustls::pki_types::CertificateDer<'static>, Arc<AtomicBool>)>,
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

    let builder =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions");
    let (builder, client_certificate_seen) = match client_auth_observation {
        Some((client_certificate, seen)) => {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(client_certificate).unwrap();
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                aioduct_test_server::tls::crypto_provider(),
            )
            .allow_unauthenticated()
            .build()
            .unwrap();
            (builder.with_client_cert_verifier(verifier), Some(seen))
        }
        None => (builder.with_no_client_auth(), None),
    };
    let mut server_config = builder
        .with_single_cert(vec![cert.cert_der.clone()], cert.key_der.clone_key())
        .unwrap();
    server_config.alpn_protocols = alpn_protocols;
    let expects_http1_alpn = !server_config.alpn_protocols.is_empty();
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
            let client_certificate_seen = client_certificate_seen.clone();
            tokio::spawn(async move {
                let mut client = match acceptor.accept(tcp).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                if let Some(seen) = client_certificate_seen {
                    seen.store(
                        client
                            .get_ref()
                            .1
                            .peer_certificates()
                            .is_some_and(|certificates| !certificates.is_empty()),
                        AtomicOrdering::SeqCst,
                    );
                }
                if expects_http1_alpn && client.get_ref().1.alpn_protocol() != Some(b"http/1.1") {
                    return;
                }
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

/// An HTTPS proxy endpoint that supports only HTTP/2. The current proxy path
/// must fail before sending textual HTTP/1.1 CONNECT bytes to it.
#[cfg(feature = "rustls")]
pub(crate) async fn tls_h2_only_proxy() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicBool>,
) {
    use tokio::io::AsyncReadExt;

    aioduct_test_server::tls::install_crypto_provider();

    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    server_config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("localhost:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let application_data_seen = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&application_data_seen);

    tokio::spawn(async move {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut tls) = acceptor.accept(tcp).await else {
            return;
        };
        let mut byte = [0u8; 1];
        if matches!(
            tokio::time::timeout(Duration::from_secs(1), tls.read(&mut byte)).await,
            Ok(Ok(n)) if n > 0
        ) {
            seen.store(true, AtomicOrdering::SeqCst);
        }
    });

    (addr, cert_der, application_data_seen)
}

#[cfg(feature = "rustls")]
pub(crate) async fn negotiated_tls_server() -> (
    std::net::SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
) {
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
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let h2 = stream.get_ref().1.alpn_protocol() == Some(b"h2");
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let service = hyper::service::service_fn(
                    |request: hyper::Request<hyper::body::Incoming>| async move {
                        Ok::<_, Infallible>(hyper::Response::new(Full::new(Bytes::from(format!(
                            "{:?}",
                            request.version()
                        )))))
                    },
                );
                if h2 {
                    let _ =
                        hyper::server::conn::http2::Builder::new(aioduct_test_server::TokioExec)
                            .serve_connection(io, service)
                            .await;
                } else {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service)
                        .await;
                }
            });
        }
    });

    (addr, cert_der)
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
