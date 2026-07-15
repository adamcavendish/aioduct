use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

pub(crate) const MULTIPART_PATH: &str = "/api/v2/ocr/jobs";
pub(crate) const TEST_TIMEOUT: Duration = Duration::from_secs(30);

const MULTIPART_BOUNDARY: &str = "aioductProxyIncomingBoundary";
const MULTIPART_FILE_PREFIX: &[u8] = b"%PDF-1.4\nproxied real Incoming upload\n";
const MULTIPART_FILE_SUFFIX: &[u8] = b"%%EOF\n";
// Large enough to exceed the proxy's constrained receive buffer repeatedly,
// without making instrumented full-suite runs spend most of their time in TLS.
const BACKPRESSURE_PADDING_BYTES: usize = 512 * 1024;
const HTTPS_PROXY_TUNNEL_CHUNK_BYTES: usize = 8 * 1024;
const HTTPS_PROXY_TUNNEL_DELAY: Duration = Duration::from_millis(2);

pub(crate) fn multipart_body() -> (String, Bytes) {
    multipart_body_with_padding(0)
}

pub(crate) fn backpressured_multipart_body() -> (String, Bytes) {
    multipart_body_with_padding(BACKPRESSURE_PADDING_BYTES)
}

fn multipart_body_with_padding(padding_bytes: usize) -> (String, Bytes) {
    let mut body = format!(
        "--{MULTIPART_BOUNDARY}\r\n\
Content-Disposition: form-data; name=\"model\"\r\n\r\n\
ocr-model\r\n\
--{MULTIPART_BOUNDARY}\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"ocr-page.pdf\"\r\n\
Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(MULTIPART_FILE_PREFIX);
    body.resize(body.len() + padding_bytes, 0xa5);
    body.extend_from_slice(MULTIPART_FILE_SUFFIX);
    body.extend_from_slice(format!("\r\n--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
    (
        format!("multipart/form-data; boundary=\"{MULTIPART_BOUNDARY}\""),
        Bytes::from(body),
    )
}

pub(crate) fn upstream_url(upstream: &http::Uri, path: &str) -> String {
    format!(
        "{}://{}{}",
        upstream
            .scheme_str()
            .expect("upstream URI should have a scheme"),
        upstream
            .authority()
            .expect("upstream URI should have an authority"),
        path
    )
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
            .expect("configured rustls provider should support default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}

#[derive(Default)]
pub(crate) struct HttpsConnectProxyObservations {
    connections: AtomicUsize,
    http1_alpn_connections: AtomicUsize,
    connect_requests: AtomicUsize,
    throttled_client_reads: AtomicUsize,
    max_tunneled_client_bytes: AtomicUsize,
}

impl HttpsConnectProxyObservations {
    pub(crate) fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub(crate) fn http1_alpn_connections(&self) -> usize {
        self.http1_alpn_connections.load(Ordering::SeqCst)
    }

    pub(crate) fn connect_requests(&self) -> usize {
        self.connect_requests.load(Ordering::SeqCst)
    }

    pub(crate) fn throttled_client_reads(&self) -> usize {
        self.throttled_client_reads.load(Ordering::SeqCst)
    }

    pub(crate) fn max_tunneled_client_bytes(&self) -> usize {
        self.max_tunneled_client_bytes.load(Ordering::SeqCst)
    }

    fn record_client_read(&self, connection_total: usize) {
        self.throttled_client_reads.fetch_add(1, Ordering::SeqCst);
        self.max_tunneled_client_bytes
            .fetch_max(connection_total, Ordering::SeqCst);
    }
}

pub(crate) struct HttpsConnectProxy {
    pub(crate) addr: SocketAddr,
    pub(crate) certificate: rustls::pki_types::CertificateDer<'static>,
    pub(crate) observations: Arc<HttpsConnectProxyObservations>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl HttpsConnectProxy {
    pub(crate) fn start() -> Self {
        aioduct_test_server::tls::install_crypto_provider();
        let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
        let certificate = cert.cert_der.clone();
        let mut config = rustls::ServerConfig::builder_with_provider(
            aioduct_test_server::tls::crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider should support default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der], cert.key_der)
        .unwrap();
        // Prefer H2 as a trap: a successful HTTP/1.1 negotiation proves that
        // the HTTPS-proxy connector restricted its ALPN offer for CONNECT.
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

        let observations = Arc::new(HttpsConnectProxyObservations::default());
        let server_observations = observations.clone();
        let (addr_tx, addr_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    addr_tx.send(listener.local_addr().unwrap()).unwrap();

                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => break,
                            accepted = listener.accept() => {
                                let (tcp, _) = accepted.unwrap();
                                socket2::SockRef::from(&tcp)
                                    .set_recv_buffer_size(HTTPS_PROXY_TUNNEL_CHUNK_BYTES)
                                    .expect("HTTPS proxy receive buffer should be configurable");
                                server_observations
                                    .connections
                                    .fetch_add(1, Ordering::SeqCst);
                                let acceptor = acceptor.clone();
                                let observations = server_observations.clone();
                                tokio::spawn(async move {
                                    let Ok(mut client) = acceptor.accept(tcp).await else {
                                        return;
                                    };
                                    if client.get_ref().1.alpn_protocol() != Some(b"http/1.1") {
                                        return;
                                    }
                                    observations
                                        .http1_alpn_connections
                                        .fetch_add(1, Ordering::SeqCst);

                                    let mut request = Vec::new();
                                    let mut chunk = [0_u8; 512];
                                    let header_end = loop {
                                        let Ok(read) = client.read(&mut chunk).await else {
                                            return;
                                        };
                                        if read == 0 || request.len() + read > 8192 {
                                            return;
                                        }
                                        request.extend_from_slice(&chunk[..read]);
                                        if let Some(end) = request
                                            .windows(4)
                                            .position(|window| window == b"\r\n\r\n")
                                        {
                                            break end + 4;
                                        }
                                    };
                                    let head = String::from_utf8_lossy(&request[..header_end]);
                                    if !head.starts_with("CONNECT ") {
                                        return;
                                    }
                                    let Some(target) = head
                                        .split_whitespace()
                                        .nth(1)
                                        .map(str::to_owned)
                                    else {
                                        return;
                                    };
                                    observations
                                        .connect_requests
                                        .fetch_add(1, Ordering::SeqCst);
                                    let Ok(upstream) = tokio::net::TcpStream::connect(target).await
                                    else {
                                        return;
                                    };
                                    if client
                                        .write_all(
                                            b"HTTP/1.1 200 Connection Established\r\n\r\n",
                                        )
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                    let prefetched = request.split_off(header_end);
                                    relay_throttled_https_tunnel(
                                        client,
                                        upstream,
                                        prefetched,
                                        observations,
                                    )
                                    .await;
                                });
                            }
                        }
                    }
                });
        });

        Self {
            addr: addr_rx
                .recv_timeout(TEST_TIMEOUT)
                .expect("HTTPS CONNECT proxy did not start"),
            certificate,
            observations,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }
}

impl Drop for HttpsConnectProxy {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("HTTPS CONNECT proxy thread panicked");
            }
        }
    }
}

async fn relay_throttled_https_tunnel(
    client: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    upstream: tokio::net::TcpStream,
    prefetched: Vec<u8>,
    observations: Arc<HttpsConnectProxyObservations>,
) {
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut upstream_read, mut upstream_write) = upstream.into_split();

    let client_to_origin = async move {
        let mut connection_total = 0;
        if !prefetched.is_empty() {
            connection_total += prefetched.len();
            observations.record_client_read(connection_total);
            tokio::time::sleep(HTTPS_PROXY_TUNNEL_DELAY).await;
            upstream_write.write_all(&prefetched).await?;
        }

        let mut chunk = vec![0_u8; HTTPS_PROXY_TUNNEL_CHUNK_BYTES];
        loop {
            let read = client_read.read(&mut chunk).await?;
            if read == 0 {
                return Ok::<_, std::io::Error>(());
            }
            connection_total += read;
            observations.record_client_read(connection_total);
            tokio::time::sleep(HTTPS_PROXY_TUNNEL_DELAY).await;
            upstream_write.write_all(&chunk[..read]).await?;
        }
    };
    let origin_to_client = async move {
        tokio::io::copy(&mut upstream_read, &mut client_write).await?;
        Ok::<_, std::io::Error>(())
    };

    tokio::select! {
        _ = client_to_origin => {}
        _ = origin_to_client => {}
    }
}

#[derive(Default)]
pub(crate) struct MultipartOriginObservations {
    connections: AtomicUsize,
    uploads: AtomicUsize,
    exact_uploads: AtomicUsize,
    file_occurrences: AtomicUsize,
}

impl MultipartOriginObservations {
    pub(crate) fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub(crate) fn uploads(&self) -> usize {
        self.uploads.load(Ordering::SeqCst)
    }

    pub(crate) fn exact_uploads(&self) -> usize {
        self.exact_uploads.load(Ordering::SeqCst)
    }

    pub(crate) fn file_occurrences(&self) -> usize {
        self.file_occurrences.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy)]
enum HttpsOriginProtocol {
    H1,
    H2,
}

impl HttpsOriginProtocol {
    fn version(self) -> http::Version {
        match self {
            Self::H1 => http::Version::HTTP_11,
            Self::H2 => http::Version::HTTP_2,
        }
    }

    fn alpn(self) -> &'static [u8] {
        match self {
            Self::H1 => b"http/1.1",
            Self::H2 => b"h2",
        }
    }
}

pub(crate) struct HttpsH1MultipartOrigin;

impl HttpsH1MultipartOrigin {
    pub(crate) fn start() -> HttpsMultipartOrigin {
        HttpsMultipartOrigin::start(HttpsOriginProtocol::H1, multipart_body())
    }

    pub(crate) fn start_backpressured() -> HttpsMultipartOrigin {
        HttpsMultipartOrigin::start(HttpsOriginProtocol::H1, backpressured_multipart_body())
    }
}

pub(crate) struct HttpsH2MultipartOrigin;

impl HttpsH2MultipartOrigin {
    pub(crate) fn start() -> HttpsMultipartOrigin {
        HttpsMultipartOrigin::start(HttpsOriginProtocol::H2, multipart_body())
    }

    pub(crate) fn start_backpressured() -> HttpsMultipartOrigin {
        HttpsMultipartOrigin::start(HttpsOriginProtocol::H2, backpressured_multipart_body())
    }
}

pub(crate) struct HttpsMultipartOrigin {
    pub(crate) addr: SocketAddr,
    pub(crate) certificate: rustls::pki_types::CertificateDer<'static>,
    pub(crate) observations: Arc<MultipartOriginObservations>,
    close_first: Arc<tokio::sync::Notify>,
    first_closed: Arc<AtomicBool>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl HttpsMultipartOrigin {
    fn start(protocol: HttpsOriginProtocol, expected_multipart: (String, Bytes)) -> Self {
        aioduct_test_server::tls::install_crypto_provider();
        let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
        let certificate = cert.cert_der.clone();
        let mut config = rustls::ServerConfig::builder_with_provider(
            aioduct_test_server::tls::crypto_provider(),
        )
        .with_safe_default_protocol_versions()
        .expect("configured rustls provider should support default TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert.cert_der], cert.key_der)
        .unwrap();
        config.alpn_protocols = vec![protocol.alpn().to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

        let observations = Arc::new(MultipartOriginObservations::default());
        let server_observations = observations.clone();
        let close_first = Arc::new(tokio::sync::Notify::new());
        let server_close_first = close_first.clone();
        let first_closed = Arc::new(AtomicBool::new(false));
        let server_first_closed = first_closed.clone();
        let (expected_content_type, expected_body) = expected_multipart;
        let (addr_tx, addr_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

        let thread = std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    addr_tx.send(listener.local_addr().unwrap()).unwrap();

                    loop {
                        tokio::select! {
                            _ = &mut shutdown_rx => break,
                            accepted = listener.accept() => {
                                let (stream, _) = accepted.unwrap();
                                let connection_id = server_observations
                                    .connections
                                    .fetch_add(1, Ordering::SeqCst)
                                    + 1;
                                let acceptor = acceptor.clone();
                                let observations = server_observations.clone();
                                let close_first = server_close_first.clone();
                                let first_closed = server_first_closed.clone();
                                let expected_content_type = expected_content_type.clone();
                                let expected_body = expected_body.clone();
                                tokio::spawn(async move {
                                    let stream = match acceptor.accept(stream).await {
                                        Ok(stream) => stream,
                                        Err(_) => return,
                                    };
                                    assert_eq!(
                                        stream.get_ref().1.alpn_protocol(),
                                        Some(protocol.alpn())
                                    );
                                    let expected_version = protocol.version();
                                    let io = aioduct_test_server::TokioIo::new(stream);
                                    match protocol {
                                        HttpsOriginProtocol::H1 => {
                                            let service = service_fn(move |request| {
                                                respond(
                                                    request,
                                                    connection_id,
                                                    expected_version,
                                                    observations.clone(),
                                                    expected_content_type.clone(),
                                                    expected_body.clone(),
                                                )
                                            });
                                            let connection = http1::Builder::new()
                                                .serve_connection(io, service);
                                            tokio::pin!(connection);

                                            if connection_id != 1 {
                                                let _ = connection.await;
                                                return;
                                            }

                                            tokio::select! {
                                                _ = &mut connection => {}
                                                _ = close_first.notified() => {
                                                    connection.as_mut().graceful_shutdown();
                                                    let _ = connection.await;
                                                }
                                            }
                                        }
                                        HttpsOriginProtocol::H2 => {
                                            let service = service_fn(move |request| {
                                                respond(
                                                    request,
                                                    connection_id,
                                                    expected_version,
                                                    observations.clone(),
                                                    expected_content_type.clone(),
                                                    expected_body.clone(),
                                                )
                                            });
                                            let connection = http2::Builder::new(
                                                aioduct_test_server::TokioExec,
                                            )
                                            .serve_connection(io, service);
                                            tokio::pin!(connection);

                                            if connection_id != 1 {
                                                let _ = connection.await;
                                                return;
                                            }

                                            tokio::select! {
                                                _ = &mut connection => {}
                                                _ = close_first.notified() => {
                                                    // Dropping the server future closes the idle
                                                    // warm tunnel without waiting for an H2 drain.
                                                }
                                            }
                                        }
                                    }
                                    first_closed.store(true, Ordering::SeqCst);
                                });
                            }
                        }
                    }
                });
        });

        Self {
            addr: addr_rx
                .recv_timeout(TEST_TIMEOUT)
                .expect("HTTPS multipart origin did not start"),
            certificate,
            observations,
            close_first,
            first_closed,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }

    pub(crate) fn upstream(&self) -> http::Uri {
        format!("https://localhost:{}", self.addr.port())
            .parse()
            .unwrap()
    }

    pub(crate) fn close_first_and_wait_blocking(&self) {
        self.close_first.notify_one();
        let deadline = Instant::now() + TEST_TIMEOUT;
        while !self.first_closed.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "warm origin connection did not close"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for HttpsMultipartOrigin {
    fn drop(&mut self) {
        self.close_first.notify_one();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("HTTPS multipart origin thread panicked");
            }
        }
    }
}

async fn respond(
    request: Request<hyper::body::Incoming>,
    connection_id: usize,
    expected_version: http::Version,
    observations: Arc<MultipartOriginObservations>,
    expected_content_type: String,
    expected_body: Bytes,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let (parts, body) = request.into_parts();
    let body = match body.collect().await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            return Ok(Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from(format!(
                    "origin body read failed: {error}"
                ))))
                .unwrap());
        }
    };

    if parts.version != expected_version {
        return Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(format!(
                "expected {expected_version:?}, got {:?}",
                parts.version
            ))))
            .unwrap());
    }

    if parts.method == http::Method::GET {
        let label = match parts.uri.path() {
            "/warm" => "warm",
            "/follow-up" => "follow-up",
            other => {
                return Ok(Response::builder()
                    .status(http::StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::from(format!("unknown path: {other}"))))
                    .unwrap());
            }
        };
        return Ok(Response::new(Full::new(Bytes::from(format!(
            "{label}:{connection_id}"
        )))));
    }

    observations.uploads.fetch_add(1, Ordering::SeqCst);
    observations.file_occurrences.fetch_add(
        body.windows(MULTIPART_FILE_PREFIX.len())
            .filter(|window| *window == MULTIPART_FILE_PREFIX)
            .count(),
        Ordering::SeqCst,
    );

    let content_lengths = parts
        .headers
        .get_all(http::header::CONTENT_LENGTH)
        .iter()
        .collect::<Vec<_>>();
    if content_lengths.len() != 1 {
        return Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(format!(
                "expected exactly one content-length, got {}",
                content_lengths.len()
            ))))
            .unwrap());
    }
    let content_length = content_lengths[0]
        .to_str()
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    if content_length != Some(expected_body.len()) {
        return Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(format!(
                "expected content-length {}, got {content_length:?}",
                expected_body.len()
            ))))
            .unwrap());
    }
    if parts.headers.contains_key(http::header::TRANSFER_ENCODING) {
        return Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from_static(
                b"multipart upload unexpectedly used transfer-encoding",
            )))
            .unwrap());
    }
    if parts.headers.contains_key(http::header::TRAILER) {
        return Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from_static(
                b"multipart upload unexpectedly declared trailers",
            )))
            .unwrap());
    }
    let exact = parts.method == http::Method::POST
        && parts.uri.path() == MULTIPART_PATH
        && parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            == Some(expected_content_type.as_str())
        && body == expected_body;
    if !exact {
        return Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(format!(
                "invalid multipart upload on connection {connection_id}"
            ))))
            .unwrap());
    }

    observations.exact_uploads.fetch_add(1, Ordering::SeqCst);
    Ok(Response::new(Full::new(Bytes::from(format!(
        "upload:{connection_id}"
    )))))
}
