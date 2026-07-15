#![cfg(feature = "rustls")]

use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub(crate) const FIRST_PROXY_HOST: &str = "first.proxy-chain.test";
pub(crate) const SECOND_PROXY_HOST: &str = "second.proxy-chain.test";
pub(crate) const ORIGIN_HOST: &str = "origin.proxy-chain.test";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProxyKind {
    Http,
    Https,
    Socks4,
    Socks4a,
    Socks5,
    Socks5h,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OriginProtocol {
    Http1,
    HttpsHttp1,
    HttpsHttp2,
}

impl OriginProtocol {
    fn scheme(self) -> &'static str {
        match self {
            Self::Http1 => "http",
            Self::HttpsHttp1 | Self::HttpsHttp2 => "https",
        }
    }

    fn expected_version(self) -> http::Version {
        match self {
            Self::Http1 | Self::HttpsHttp1 => http::Version::HTTP_11,
            Self::HttpsHttp2 => http::Version::HTTP_2,
        }
    }
}

impl ProxyKind {
    const ALL: [Self; 6] = [
        Self::Http,
        Self::Https,
        Self::Socks4,
        Self::Socks4a,
        Self::Socks5,
        Self::Socks5h,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Https => "HTTPS",
            Self::Socks4 => "SOCKS4",
            Self::Socks4a => "SOCKS4a",
            Self::Socks5 => "SOCKS5",
            Self::Socks5h => "SOCKS5h",
        }
    }
}

pub(crate) fn ordered_proxy_pairs() -> Vec<(ProxyKind, ProxyKind)> {
    ProxyKind::ALL
        .into_iter()
        .flat_map(|first| {
            ProxyKind::ALL
                .into_iter()
                .map(move |second| (first, second))
        })
        .collect()
}

pub(crate) fn assert_complete_ordered_pairs(cases: &[(ProxyKind, ProxyKind)]) {
    assert_eq!(cases.len(), 36);
    for first in ProxyKind::ALL {
        for second in ProxyKind::ALL {
            assert_eq!(
                cases
                    .iter()
                    .filter(|&&(candidate_first, candidate_second)| {
                        candidate_first == first && candidate_second == second
                    })
                    .count(),
                1,
                "proxy matrix must contain {first:?} -> {second:?} exactly once"
            );
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WireEvent {
    Negotiated {
        hop: u8,
        kind: ProxyKind,
        target: String,
    },
    OriginBytes {
        version: http::Version,
    },
}

struct StartedProxy {
    addr: SocketAddr,
    certificate: Option<rustls::pki_types::CertificateDer<'static>>,
}

struct StartedOrigin {
    addr: SocketAddr,
    certificate: Option<rustls::pki_types::CertificateDer<'static>>,
}

struct Ready {
    origin: StartedOrigin,
    first: StartedProxy,
    second: StartedProxy,
}

pub(crate) struct LiveMixedProxyChain {
    first_kind: ProxyKind,
    second_kind: ProxyKind,
    origin_protocol: OriginProtocol,
    origin_addr: SocketAddr,
    first_addr: SocketAddr,
    second_addr: SocketAddr,
    certificates: Vec<rustls::pki_types::CertificateDer<'static>>,
    events: Arc<Mutex<Vec<WireEvent>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    server_thread: Option<std::thread::JoinHandle<()>>,
}

impl LiveMixedProxyChain {
    pub(crate) fn start_with_origin(
        first_kind: ProxyKind,
        second_kind: ProxyKind,
        origin_protocol: OriginProtocol,
    ) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let server_events = Arc::clone(&events);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let origin = start_origin(origin_protocol, Arc::clone(&server_events)).await;
                let second = start_proxy(
                    second_kind,
                    2,
                    SECOND_PROXY_HOST,
                    Arc::clone(&server_events),
                )
                .await;
                let first =
                    start_proxy(first_kind, 1, FIRST_PROXY_HOST, Arc::clone(&server_events)).await;
                ready_tx
                    .send(Ready {
                        origin,
                        first,
                        second,
                    })
                    .unwrap();
                let _ = shutdown_rx.await;
            });
        });
        let ready = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("mixed proxy chain did not start");
        let certificates = [
            ready.first.certificate.clone(),
            ready.second.certificate.clone(),
            ready.origin.certificate.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();

        Self {
            first_kind,
            second_kind,
            origin_protocol,
            origin_addr: ready.origin.addr,
            first_addr: ready.first.addr,
            second_addr: ready.second.addr,
            certificates,
            events,
            shutdown: Some(shutdown_tx),
            server_thread: Some(server_thread),
        }
    }

    pub(crate) fn first_proxy(&self) -> aioduct::ProxyConfig {
        proxy_config(self.first_kind, FIRST_PROXY_HOST, self.first_addr.port())
    }

    pub(crate) fn second_proxy(&self) -> aioduct::ProxyConfig {
        proxy_config(self.second_kind, SECOND_PROXY_HOST, self.second_addr.port())
    }

    pub(crate) fn origin_url(&self) -> String {
        format!(
            "{}://{ORIGIN_HOST}:{}/mixed-chain",
            self.origin_protocol.scheme(),
            self.origin_addr.port()
        )
    }

    pub(crate) fn first_addr(&self) -> SocketAddr {
        self.first_addr
    }

    pub(crate) fn second_addr(&self) -> SocketAddr {
        self.second_addr
    }

    pub(crate) fn origin_addr(&self) -> SocketAddr {
        self.origin_addr
    }

    pub(crate) fn certificates(&self) -> &[rustls::pki_types::CertificateDer<'static>] {
        &self.certificates
    }

    pub(crate) fn assert_wire_order(&self) {
        let expected = vec![
            WireEvent::Negotiated {
                hop: 1,
                kind: self.first_kind,
                target: expected_target(
                    self.first_kind,
                    SECOND_PROXY_HOST,
                    self.second_addr.port(),
                ),
            },
            WireEvent::Negotiated {
                hop: 2,
                kind: self.second_kind,
                target: expected_target(self.second_kind, ORIGIN_HOST, self.origin_addr.port()),
            },
            WireEvent::OriginBytes {
                version: self.origin_protocol.expected_version(),
            },
        ];
        assert_eq!(
            *self.events.lock().unwrap(),
            expected,
            "unexpected wire order for {} -> {}",
            self.first_kind.label(),
            self.second_kind.label()
        );
    }
}

impl Drop for LiveMixedProxyChain {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(server_thread) = self.server_thread.take() {
            let _ = server_thread.join();
        }
    }
}

pub(crate) fn client_config_trusting(
    certificates: &[rustls::pki_types::CertificateDer<'static>],
) -> Arc<rustls::ClientConfig> {
    aioduct_test_server::tls::install_crypto_provider();
    let mut roots = rustls::RootCertStore::empty();
    for certificate in certificates {
        roots.add(certificate.clone()).unwrap();
    }
    let mut config =
        rustls::ClientConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}

fn proxy_config(kind: ProxyKind, host: &str, port: u16) -> aioduct::ProxyConfig {
    let uri = format!("{}://{host}:{port}", proxy_scheme(kind));
    match kind {
        ProxyKind::Http => aioduct::ProxyConfig::http(&uri).unwrap(),
        ProxyKind::Https => aioduct::ProxyConfig::https(&uri).unwrap(),
        ProxyKind::Socks4 => aioduct::ProxyConfig::socks4(&uri).unwrap(),
        ProxyKind::Socks4a => aioduct::ProxyConfig::socks4(&uri).unwrap(),
        ProxyKind::Socks5 => aioduct::ProxyConfig::socks5(&uri).unwrap(),
        ProxyKind::Socks5h => aioduct::ProxyConfig::socks5h(&uri).unwrap(),
    }
}

fn proxy_scheme(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Http => "http",
        ProxyKind::Https => "https",
        ProxyKind::Socks4 => "socks4",
        ProxyKind::Socks4a => "socks4a",
        ProxyKind::Socks5 => "socks5",
        ProxyKind::Socks5h => "socks5h",
    }
}

fn expected_target(kind: ProxyKind, remote_host: &str, port: u16) -> String {
    match kind {
        ProxyKind::Http | ProxyKind::Https | ProxyKind::Socks4a | ProxyKind::Socks5h => {
            format!("{remote_host}:{port}")
        }
        ProxyKind::Socks4 | ProxyKind::Socks5 => format!("127.0.0.1:{port}"),
    }
}

async fn start_origin(
    protocol: OriginProtocol,
    events: Arc<Mutex<Vec<WireEvent>>>,
) -> StartedOrigin {
    if protocol == OriginProtocol::Http1 {
        let (addr, _) = aioduct_test_server::h1::h1_server_with(move |request| {
            record_origin_request(&events, &request);
            async move { mixed_chain_response() }
        })
        .await;
        return StartedOrigin {
            addr,
            certificate: None,
        };
    }

    aioduct_test_server::tls::install_crypto_provider();
    let certificate = aioduct_test_server::tls::generate_self_signed(&[ORIGIN_HOST]);
    let certificate_der = certificate.cert_der.clone();
    let mut config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.cert_der], certificate.key_der)
            .unwrap();
    config.alpn_protocols = match protocol {
        OriginProtocol::HttpsHttp1 => vec![b"http/1.1".to_vec()],
        OriginProtocol::HttpsHttp2 => vec![b"h2".to_vec()],
        OriginProtocol::Http1 => unreachable!(),
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let events = Arc::clone(&events);
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let io = aioduct_test_server::TokioIo::new(stream);
                match protocol {
                    OriginProtocol::HttpsHttp1 => {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(
                                io,
                                service_fn(move |request| {
                                    record_origin_request(&events, &request);
                                    async move { mixed_chain_response() }
                                }),
                            )
                            .await;
                    }
                    OriginProtocol::HttpsHttp2 => {
                        let _ = hyper::server::conn::http2::Builder::new(
                            aioduct_test_server::TokioExec,
                        )
                        .serve_connection(
                            io,
                            service_fn(move |request| {
                                record_origin_request(&events, &request);
                                async move { mixed_chain_response() }
                            }),
                        )
                        .await;
                    }
                    OriginProtocol::Http1 => unreachable!(),
                }
            });
        }
    });

    StartedOrigin {
        addr,
        certificate: Some(certificate_der),
    }
}

fn record_origin_request(
    events: &Arc<Mutex<Vec<WireEvent>>>,
    request: &Request<hyper::body::Incoming>,
) {
    events.lock().unwrap().push(WireEvent::OriginBytes {
        version: request.version(),
    });
}

fn mixed_chain_response() -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from_static(
        b"mixed-chain-ok",
    ))))
}

async fn start_proxy(
    kind: ProxyKind,
    hop: u8,
    certificate_name: &'static str,
    events: Arc<Mutex<Vec<WireEvent>>>,
) -> StartedProxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    if kind == ProxyKind::Https {
        aioduct_test_server::tls::install_crypto_provider();
        let certificate = aioduct_test_server::tls::generate_self_signed(&[certificate_name]);
        let certificate_der = certificate.cert_der.clone();
        let mut config = rustls::ServerConfig::builder_with_provider(
            aioduct_test_server::tls::crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.cert_der], certificate.key_der)
        .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let events = Arc::clone(&events);
                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let _ = serve_proxy(stream, kind, hop, events).await;
                });
            }
        });
        StartedProxy {
            addr,
            certificate: Some(certificate_der),
        }
    } else {
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let events = Arc::clone(&events);
                tokio::spawn(async move {
                    let _ = serve_proxy(stream, kind, hop, events).await;
                });
            }
        });
        StartedProxy {
            addr,
            certificate: None,
        }
    }
}

async fn serve_proxy<S>(
    mut client: S,
    kind: ProxyKind,
    hop: u8,
    events: Arc<Mutex<Vec<WireEvent>>>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let target = match kind {
        ProxyKind::Http | ProxyKind::Https => read_http_connect(&mut client).await?,
        ProxyKind::Socks4 | ProxyKind::Socks4a => read_socks4_connect(&mut client).await?,
        ProxyKind::Socks5 => read_socks5_connect(&mut client, false).await?,
        ProxyKind::Socks5h => read_socks5_connect(&mut client, true).await?,
    };
    let mut upstream = TcpStream::connect((Ipv4Addr::LOCALHOST, target.port)).await?;
    events.lock().unwrap().push(WireEvent::Negotiated {
        hop,
        kind,
        target: format!("{}:{}", target.host, target.port),
    });
    match kind {
        ProxyKind::Http | ProxyKind::Https => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }
        ProxyKind::Socks4 | ProxyKind::Socks4a => {
            client.write_all(&[0, 0x5a, 0, 0, 0, 0, 0, 0]).await?;
        }
        ProxyKind::Socks5 | ProxyKind::Socks5h => {
            client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        }
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

struct ProxyTarget {
    host: String,
    port: u16,
}

async fn read_http_connect<S>(stream: &mut S) -> io::Result<ProxyTarget>
where
    S: AsyncRead + Unpin,
{
    let mut request = Vec::new();
    let mut chunk = [0_u8; 512];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).await?;
        if count == 0 || request.len() + count > 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete CONNECT request",
            ));
        }
        request.extend_from_slice(&chunk[..count]);
    }
    let head = String::from_utf8_lossy(&request);
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    if parts.next() != Some("CONNECT") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected CONNECT request",
        ));
    }
    parse_authority(parts.next().unwrap_or_default())
}

async fn read_socks4_connect<S>(stream: &mut S) -> io::Result<ProxyTarget>
where
    S: AsyncRead + Unpin,
{
    let mut request = [0_u8; 8];
    stream.read_exact(&mut request).await?;
    if request[..2] != [4, 1] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected SOCKS4 CONNECT request",
        ));
    }
    read_nul_terminated(stream).await?;
    let host = if request[4..8] == [0, 0, 0, 1] {
        String::from_utf8(read_nul_terminated(stream).await?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    } else {
        Ipv4Addr::new(request[4], request[5], request[6], request[7]).to_string()
    };
    Ok(ProxyTarget {
        host,
        port: u16::from_be_bytes([request[2], request[3]]),
    })
}

async fn read_socks5_connect<S>(stream: &mut S, remote_dns: bool) -> io::Result<ProxyTarget>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut greeting = [0_u8; 2];
    stream.read_exact(&mut greeting).await?;
    if greeting[0] != 5 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected SOCKS5 greeting",
        ));
    }
    let mut methods = vec![0_u8; greeting[1] as usize];
    stream.read_exact(&mut methods).await?;
    stream.write_all(&[5, 0]).await?;

    let mut request = [0_u8; 4];
    stream.read_exact(&mut request).await?;
    if request[..3] != [5, 1, 0] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected SOCKS5 CONNECT request",
        ));
    }
    if remote_dns != (request[3] == 3) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected SOCKS5 address representation",
        ));
    }
    let host = match request[3] {
        1 => {
            let mut octets = [0_u8; 4];
            stream.read_exact(&mut octets).await?;
            Ipv4Addr::from(octets).to_string()
        }
        3 => {
            let length = stream.read_u8().await? as usize;
            let mut name = vec![0_u8; length];
            stream.read_exact(&mut name).await?;
            String::from_utf8(name)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        }
        4 => {
            let mut octets = [0_u8; 16];
            stream.read_exact(&mut octets).await?;
            Ipv6Addr::from(octets).to_string()
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported SOCKS5 address type",
            ));
        }
    };
    Ok(ProxyTarget {
        host,
        port: stream.read_u16().await?,
    })
}

async fn read_nul_terminated<S>(stream: &mut S) -> io::Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut value = Vec::new();
    while value.len() <= 1024 {
        let byte = stream.read_u8().await?;
        if byte == 0 {
            return Ok(value);
        }
        value.push(byte);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "unterminated SOCKS4 field",
    ))
}

fn parse_authority(authority: &str) -> io::Result<ProxyTarget> {
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CONNECT target has no port"))?;
    Ok(ProxyTarget {
        host: host.to_owned(),
        port: port
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    })
}
