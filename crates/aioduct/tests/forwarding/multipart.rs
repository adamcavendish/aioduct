use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::TokioExec;
use bytes::Bytes;
use http::header::{CONTENT_LENGTH, CONTENT_TYPE, TRAILER, TRANSFER_ENCODING};
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1 as server_http1;
use hyper::server::conn::http2 as server_http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MULTIPART_BOUNDARY: &str = "aioductBoundary7MA4YWxkTrZu0gW";
const MULTIPART_FILE_BYTES: &[u8] = b"%PDF-1.4\naioduct forwarded upload\n%%EOF\n";

fn multipart_upload_body() -> (String, Bytes) {
    let body = format!(
        "--{boundary}\r\n\
Content-Disposition: form-data; name=\"model\"\r\n\r\n\
ocr-model\r\n\
--{boundary}\r\n\
Content-Disposition: form-data; name=\"optionalPayload\"\r\n\r\n\
{{\"source\":\"aioduct-test\"}}\r\n\
--{boundary}\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"ocr-page.pdf\"\r\n\
Content-Type: application/octet-stream\r\n\r\n",
        boundary = MULTIPART_BOUNDARY,
    );
    let mut bytes = body.into_bytes();
    bytes.extend_from_slice(MULTIPART_FILE_BYTES);
    bytes.extend_from_slice(format!("\r\n--{}--\r\n", MULTIPART_BOUNDARY).as_bytes());
    (
        format!("multipart/form-data; boundary=\"{}\"", MULTIPART_BOUNDARY),
        Bytes::from(bytes),
    )
}

fn byte_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn validate_multipart_parts(
    method: &http::Method,
    uri: &http::Uri,
    version: http::Version,
    headers: &http::HeaderMap,
    body: &[u8],
    expected_path: &str,
    expected_version: http::Version,
) -> Result<(), String> {
    if method != http::Method::POST {
        return Err(format!("expected POST, got {method}"));
    }
    if uri.path() != expected_path {
        return Err(format!("expected path {expected_path}, got {uri}"));
    }
    if version != expected_version {
        return Err(format!(
            "expected upstream version {expected_version:?}, got {version:?}"
        ));
    }

    let (expected_content_type, expected_body) = multipart_upload_body();
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "missing or invalid content-type".to_owned())?;
    if content_type != expected_content_type {
        return Err(format!(
            "expected content-type {expected_content_type}, got {content_type}"
        ));
    }

    let content_lengths = headers.get_all(CONTENT_LENGTH).iter().collect::<Vec<_>>();
    if content_lengths.len() != 1 {
        return Err(format!(
            "expected exactly one content-length, got {}",
            content_lengths.len()
        ));
    }
    let content_length = content_lengths[0]
        .to_str()
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "invalid content-length".to_owned())?;
    if content_length != expected_body.len() {
        return Err(format!(
            "expected content-length {}, got {content_length}",
            expected_body.len()
        ));
    }
    if headers.contains_key(TRANSFER_ENCODING) {
        return Err(format!(
            "{expected_version:?} multipart upload unexpectedly used transfer-encoding"
        ));
    }
    if headers.contains_key(TRAILER) {
        return Err(format!(
            "{expected_version:?} multipart upload unexpectedly declared trailers"
        ));
    }

    let file_occurrences = byte_occurrences(body, MULTIPART_FILE_BYTES);
    if file_occurrences != 1 {
        return Err(format!(
            "expected file bytes exactly once, got {file_occurrences} occurrences"
        ));
    }
    if body != expected_body {
        let mismatch = body
            .iter()
            .zip(expected_body.iter())
            .position(|(actual, expected)| actual != expected)
            .unwrap_or_else(|| body.len().min(expected_body.len()));
        return Err(format!(
            "multipart body differs at byte {mismatch}: expected {} bytes, got {}",
            expected_body.len(),
            body.len()
        ));
    }
    Ok(())
}

async fn validate_forwarded_multipart(
    req: Request<hyper::body::Incoming>,
    expected_path: &str,
    expected_version: http::Version,
) -> Result<(), String> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map_err(|error| format!("upstream body read failed: {error}"))?
        .to_bytes();
    validate_multipart_parts(
        &parts.method,
        &parts.uri,
        parts.version,
        &parts.headers,
        &body,
        expected_path,
        expected_version,
    )
}

async fn multipart_validation_response(
    req: Request<hyper::body::Incoming>,
    marker: &'static str,
    expected_version: http::Version,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match validate_forwarded_multipart(req, "/api/v2/ocr/jobs", expected_version).await {
        Ok(()) => Ok(Response::new(Full::new(Bytes::from_static(
            marker.as_bytes(),
        )))),
        Err(error) => Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(error)))
            .unwrap()),
    }
}

fn identified_probe_body(label: &str, connection_id: usize) -> Bytes {
    Bytes::from(format!("{label}:connection-{connection_id}"))
}

async fn identified_multipart_or_pool_probe_response(
    req: Request<hyper::body::Incoming>,
    marker: &'static str,
    connection_id: usize,
    expected_version: http::Version,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if req.method() == http::Method::GET {
        let label = match req.uri().path() {
            "/warm" => Some("warm"),
            "/follow-up" => Some("follow-up"),
            _ => None,
        };
        if let Some(label) = label {
            return Ok(Response::new(Full::new(identified_probe_body(
                label,
                connection_id,
            ))));
        }
    }

    match validate_forwarded_multipart(req, "/api/v2/ocr/jobs", expected_version).await {
        Ok(()) => Ok(Response::new(Full::new(identified_probe_body(
            marker,
            connection_id,
        )))),
        Err(error) => Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(error)))
            .unwrap()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedRequest {
    method: http::Method,
    path: String,
    version: http::Version,
}

impl ObservedRequest {
    fn new(method: http::Method, path: &str, version: http::Version) -> Self {
        Self {
            method,
            path: path.to_owned(),
            version,
        }
    }
}

#[derive(Default)]
struct PhysicalConnectionObservationState {
    connections: AtomicUsize,
    requests: Mutex<BTreeMap<usize, Vec<ObservedRequest>>>,
}

#[derive(Clone, Default)]
struct PhysicalConnectionObservations(Arc<PhysicalConnectionObservationState>);

impl PhysicalConnectionObservations {
    fn accept_connection(&self) -> usize {
        self.0.connections.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn record_request<B>(&self, connection_id: usize, request: &Request<B>) {
        self.record(
            connection_id,
            request.method().clone(),
            request.uri().path(),
            request.version(),
        );
    }

    fn record(
        &self,
        connection_id: usize,
        method: http::Method,
        path: &str,
        version: http::Version,
    ) {
        self.0
            .requests
            .lock()
            .unwrap()
            .entry(connection_id)
            .or_default()
            .push(ObservedRequest::new(method, path, version));
    }

    fn connections(&self) -> usize {
        self.0.connections.load(Ordering::SeqCst)
    }

    fn requests(&self, connection_id: usize) -> Vec<ObservedRequest> {
        self.0
            .requests
            .lock()
            .unwrap()
            .get(&connection_id)
            .cloned()
            .unwrap_or_default()
    }
}

async fn wait_for_first_connection_unusable(
    signal: tokio::sync::oneshot::Receiver<()>,
    protocol: &str,
) {
    tokio::time::timeout(Duration::from_secs(5), signal)
        .await
        .unwrap_or_else(|_| panic!("{protocol} first connection did not become unusable"))
        .unwrap_or_else(|_| panic!("{protocol} first-connection signal was dropped"));
}

fn assert_stale_connection_observations(
    observations: &PhysicalConnectionObservations,
    upload_version: http::Version,
) {
    assert_eq!(observations.connections(), 2);
    assert_eq!(
        observations.requests(1),
        vec![ObservedRequest::new(
            http::Method::GET,
            "/warm",
            upload_version,
        )],
        "the old physical connection must receive only the warm request"
    );
    assert_eq!(
        observations.requests(2),
        vec![ObservedRequest::new(
            http::Method::POST,
            "/api/v2/ocr/jobs",
            upload_version,
        )],
        "the replacement physical connection must receive the upload exactly once"
    );
}

async fn start_identified_http1_multipart_upstream(
    marker: &'static str,
) -> (SocketAddr, aioduct_test_server::ConnectionCounter) {
    let counter = aioduct_test_server::ConnectionCounter::new();
    let counter2 = counter.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let connection_id = counter2.inc_connections() + 1;
            let request_counter = counter2.clone();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            request_counter.inc_requests();
                            identified_multipart_or_pool_probe_response(
                                req,
                                marker,
                                connection_id,
                                http::Version::HTTP_11,
                            )
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, counter)
}

async fn start_identified_h2c_multipart_upstream(
    marker: &'static str,
) -> (SocketAddr, aioduct_test_server::ConnectionCounter) {
    let counter = aioduct_test_server::ConnectionCounter::new();
    let counter2 = counter.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let connection_id = counter2.inc_connections() + 1;
            let request_counter = counter2.clone();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            request_counter.inc_requests();
                            identified_multipart_or_pool_probe_response(
                                req,
                                marker,
                                connection_id,
                                http::Version::HTTP_2,
                            )
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, counter)
}

#[cfg(feature = "rustls")]
async fn start_identified_tls_h2_multipart_upstream(
    marker: &'static str,
) -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    aioduct_test_server::ConnectionCounter,
) {
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let counter = aioduct_test_server::ConnectionCounter::new();
    let counter2 = counter.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let connection_id = counter2.inc_connections() + 1;
            let request_counter = counter2.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(stream) => stream,
                    Err(_) => return,
                };
                let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);
                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            request_counter.inc_requests();
                            identified_multipart_or_pool_probe_response(
                                req,
                                marker,
                                connection_id,
                                http::Version::HTTP_2,
                            )
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, cert_der, counter)
}

async fn start_http_multipart_upstream(marker: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            multipart_validation_response(req, marker, http::Version::HTTP_11)
                        }),
                    )
                    .await;
            });
        }
    });

    addr
}

async fn read_raw_headers(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while request.len() < 16 * 1024 {
        let n = stream.read(&mut buf).await.unwrap();
        assert_ne!(n, 0, "peer closed before request headers completed");
        request.extend_from_slice(&buf[..n]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return request;
        }
    }
    panic!("request headers exceeded 16 KiB");
}

async fn start_http1_stale_multipart_upstream(
    marker: &'static str,
) -> (
    SocketAddr,
    PhysicalConnectionObservations,
    tokio::sync::oneshot::Receiver<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let observations = PhysicalConnectionObservations::default();
    let server_observations = observations.clone();
    let (first_unusable_tx, first_unusable_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let mut first_unusable_tx = Some(first_unusable_tx);
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let connection_id = server_observations.accept_connection();
            let observations = server_observations.clone();
            let first_unusable = if connection_id == 1 {
                first_unusable_tx.take()
            } else {
                None
            };
            tokio::spawn(async move {
                if connection_id == 1 {
                    let headers = read_raw_headers(&mut stream).await;
                    assert!(
                        headers.starts_with(b"GET /warm HTTP/1.1\r\n"),
                        "unexpected warm request: {}",
                        String::from_utf8_lossy(&headers)
                    );
                    observations.record(
                        connection_id,
                        http::Method::GET,
                        "/warm",
                        http::Version::HTTP_11,
                    );
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\nwarm",
                        )
                        .await
                        .unwrap();
                    stream.flush().await.unwrap();
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                    first_unusable.unwrap().send(()).unwrap();
                    return;
                }

                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            observations.record_request(connection_id, &req);
                            multipart_validation_response(req, marker, http::Version::HTTP_11)
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, observations, first_unusable_rx)
}

#[cfg(feature = "rustls")]
async fn start_tls_h2_goaway_multipart_upstream(
    marker: &'static str,
) -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    PhysicalConnectionObservations,
    tokio::sync::oneshot::Receiver<()>,
) {
    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();
    let mut config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("configured rustls provider does not support the default TLS versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let observations = PhysicalConnectionObservations::default();
    let server_observations = observations.clone();
    let (first_unusable_tx, first_unusable_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let mut first_unusable_tx = Some(first_unusable_tx);
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let connection_id = server_observations.accept_connection();
            let observations = server_observations.clone();
            let first_unusable = if connection_id == 1 {
                first_unusable_tx.take()
            } else {
                None
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(stream) => stream,
                    Err(_) => return,
                };
                let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);

                if connection_id == 1 {
                    let (warm_seen_tx, mut warm_seen_rx) = tokio::sync::oneshot::channel();
                    let warm_seen_tx = Arc::new(Mutex::new(Some(warm_seen_tx)));
                    let conn = server_http2::Builder::new(TokioExec).serve_connection(
                        io,
                        service_fn(move |req: Request<hyper::body::Incoming>| {
                            observations.record_request(connection_id, &req);
                            let warm_seen_tx = warm_seen_tx.clone();
                            async move {
                                if req.method() == http::Method::GET {
                                    warm_seen_tx
                                        .lock()
                                        .unwrap()
                                        .take()
                                        .unwrap()
                                        .send(())
                                        .unwrap();
                                    Ok::<_, Infallible>(Response::new(Full::new(
                                        Bytes::from_static(b"warm"),
                                    )))
                                } else {
                                    multipart_validation_response(
                                        req,
                                        marker,
                                        http::Version::HTTP_2,
                                    )
                                    .await
                                }
                            }
                        }),
                    );
                    tokio::pin!(conn);
                    tokio::select! {
                        result = &mut conn => {
                            let _ = result;
                        }
                        warm_seen = &mut warm_seen_rx => {
                            warm_seen.expect("warm request signal was dropped");
                            conn.as_mut().graceful_shutdown();
                            let _ = conn.await;
                        }
                    }
                    first_unusable.unwrap().send(()).unwrap();
                    return;
                }

                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            observations.record_request(connection_id, &req);
                            multipart_validation_response(req, marker, http::Version::HTTP_2)
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, cert_der, observations, first_unusable_rx)
}

#[cfg(all(feature = "rustls", feature = "http3"))]
fn multipart_validation_h3_response(
    req: http::Request<()>,
    body: Bytes,
    marker: &'static str,
) -> (http::StatusCode, Bytes) {
    match validate_multipart_parts(
        req.method(),
        req.uri(),
        req.version(),
        req.headers(),
        &body,
        "/api/v2/ocr/jobs",
        http::Version::HTTP_3,
    ) {
        Ok(()) => (http::StatusCode::OK, Bytes::from_static(marker.as_bytes())),
        Err(error) => (http::StatusCode::BAD_REQUEST, Bytes::from(error)),
    }
}

#[cfg(all(feature = "rustls", feature = "http3"))]
async fn start_h3_closed_after_warm_multipart_upstream(
    marker: &'static str,
) -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    PhysicalConnectionObservations,
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    aioduct_test_server::tls::install_crypto_provider();

    let cert = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let cert_der = cert.cert_der.clone();

    let mut server_tls_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert_der], cert.key_der)
            .unwrap();
    server_tls_config.alpn_protocols = vec![b"h3".to_vec()];
    server_tls_config.max_early_data_size = 0;

    let server_config = h3_quinn::quinn::ServerConfig::with_crypto(Arc::new(
        h3_quinn::quinn::crypto::rustls::QuicServerConfig::try_from(server_tls_config).unwrap(),
    ));

    let endpoint =
        h3_quinn::quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();
    let observations = PhysicalConnectionObservations::default();
    let server_observations = observations.clone();
    let (close_first_tx, close_first_rx) = tokio::sync::oneshot::channel();
    let (first_unusable_tx, first_unusable_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let mut close_first_rx = Some(close_first_rx);
        let mut first_unusable_tx = Some(first_unusable_tx);
        while let Some(incoming) = endpoint.accept().await {
            let connection_id = server_observations.accept_connection();
            let observations = server_observations.clone();
            let close_first = if connection_id == 1 {
                close_first_rx.take()
            } else {
                None
            };
            let first_unusable = if connection_id == 1 {
                first_unusable_tx.take()
            } else {
                None
            };
            tokio::spawn(async move {
                let quinn_conn = match incoming.await {
                    Ok(conn) => conn,
                    Err(_) => return,
                };
                let close_conn = quinn_conn.clone();
                let mut h3_conn: h3::server::Connection<h3_quinn::Connection, Bytes> =
                    match h3::server::Connection::new(h3_quinn::Connection::new(quinn_conn)).await {
                        Ok(conn) => conn,
                        Err(_) => return,
                    };

                if connection_id == 1 {
                    let resolver = match h3_conn.accept().await {
                        Ok(Some(resolver)) => resolver,
                        _ => return,
                    };
                    let (req, mut stream) = match resolver.resolve_request().await {
                        Ok(resolved) => resolved,
                        Err(_) => return,
                    };
                    observations.record_request(connection_id, &req);
                    while let Some(mut chunk) = stream.recv_data().await.unwrap_or(None) {
                        use bytes::Buf;
                        chunk.advance(chunk.remaining());
                    }
                    let response = http::Response::builder()
                        .status(http::StatusCode::OK)
                        .body(())
                        .unwrap();
                    if stream.send_response(response).await.is_err() {
                        return;
                    }
                    let _ = stream.send_data(Bytes::from_static(b"warm")).await;
                    let _ = stream.finish().await;
                    drop(stream);
                    close_first
                        .unwrap()
                        .await
                        .expect("test should request closure after receiving the warm response");
                    close_conn.close(h3_quinn::quinn::VarInt::from_u32(0), b"warm complete");
                    drop(h3_conn);
                    drop(close_conn);
                    first_unusable.unwrap().send(()).unwrap();
                    return;
                }

                loop {
                    let resolver = match h3_conn.accept().await {
                        Ok(Some(resolver)) => resolver,
                        Ok(None) | Err(_) => break,
                    };
                    let request_observations = observations.clone();
                    tokio::spawn(async move {
                        let (req, mut stream) = match resolver.resolve_request().await {
                            Ok(resolved) => resolved,
                            Err(_) => return,
                        };
                        request_observations.record_request(connection_id, &req);

                        let mut body_buf = Vec::new();
                        while let Some(mut chunk) = stream.recv_data().await.unwrap_or(None) {
                            use bytes::Buf;
                            body_buf.extend_from_slice(chunk.chunk());
                            chunk.advance(chunk.remaining());
                        }

                        let (status, resp_body) =
                            multipart_validation_h3_response(req, Bytes::from(body_buf), marker);
                        let response = http::Response::builder().status(status).body(()).unwrap();
                        if stream.send_response(response).await.is_err() {
                            return;
                        }
                        if !resp_body.is_empty() {
                            let _ = stream.send_data(resp_body).await;
                        }
                        let _ = stream.finish().await;
                    });
                }
            });
        }
    });

    (
        addr,
        cert_der,
        observations,
        close_first_tx,
        first_unusable_rx,
    )
}

#[derive(Clone, Copy)]
enum ForwardProtocol {
    Auto,
    H2c,
}

async fn start_forwarding_broker(
    client: HttpEngineSend<TokioRuntime, TcpConnector>,
    upstream: http::Uri,
) -> SocketAddr {
    start_forwarding_broker_with_protocol(client, upstream, ForwardProtocol::Auto).await
}

async fn start_forwarding_broker_with_protocol(
    client: HttpEngineSend<TokioRuntime, TcpConnector>,
    upstream: http::Uri,
    protocol: ForwardProtocol,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            let client = client.clone();
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req: Request<hyper::body::Incoming>| {
                            let client = client.clone();
                            let upstream = upstream.clone();
                            let protocol = protocol;
                            async move {
                                let mut forward = client.forward(req).upstream(upstream);
                                if matches!(protocol, ForwardProtocol::H2c) {
                                    forward = forward.h2c();
                                }
                                let response = match forward.send().await {
                                    Ok(resp) => {
                                        let status = resp.status();
                                        match resp.bytes().await {
                                            Ok(body) => Response::builder()
                                                .status(status)
                                                .body(Full::new(body))
                                                .unwrap(),
                                            Err(error) => Response::builder()
                                                .status(http::StatusCode::BAD_GATEWAY)
                                                .body(Full::new(Bytes::from(error.to_string())))
                                                .unwrap(),
                                        }
                                    }
                                    Err(error) => Response::builder()
                                        .status(http::StatusCode::BAD_GATEWAY)
                                        .body(Full::new(Bytes::from(error.to_string())))
                                        .unwrap(),
                                };
                                Ok::<_, Infallible>(response)
                            }
                        }),
                    )
                    .await;
            });
        }
    });

    addr
}

async fn post_raw_multipart(addr: SocketAddr) -> (u16, Bytes) {
    post_raw_multipart_with_version(addr, "HTTP/1.1").await
}

async fn post_raw_multipart_with_version(addr: SocketAddr, version: &str) -> (u16, Bytes) {
    let (content_type, body) = multipart_upload_body();
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let headers = format!(
        "POST /api/v2/ocr/jobs {version}\r\n\
Host: {addr}\r\n\
Content-Type: {content_type}\r\n\
Content-Length: {len}\r\n\
Connection: close\r\n\r\n",
        len = body.len(),
    );

    stream.write_all(headers.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    stream.flush().await.unwrap();

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("raw response should include header terminator");
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .expect("raw response should include status code");
    (status, Bytes::copy_from_slice(&response[header_end + 4..]))
}

#[tokio::test]
async fn forward_real_incoming_multipart_http11_to_http1_upstream() {
    let upstream_addr = start_http_multipart_upstream("multipart-ok:http").await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let broker_addr = start_forwarding_broker(
        client,
        format!("http://{upstream_addr}")
            .parse::<http::Uri>()
            .unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:http"));
}

#[tokio::test]
async fn forward_real_incoming_multipart_http10_to_http1_upstream() {
    let upstream_addr = start_http_multipart_upstream("multipart-ok:http10").await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let broker_addr = start_forwarding_broker(
        client,
        format!("http://{upstream_addr}")
            .parse::<http::Uri>()
            .unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart_with_version(broker_addr, "HTTP/1.0").await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:http10"));
}

#[tokio::test]
async fn forward_real_incoming_multipart_to_http1_upstream_succeeds_after_closed_pool_connection() {
    let (upstream_addr, observations, first_unusable) =
        start_http1_stale_multipart_upstream("multipart-ok:http1-stale").await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let upstream_origin = format!("http://{upstream_addr}");

    let warm = client
        .get(&format!("{upstream_origin}/warm"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.text().await.unwrap(), "warm");
    wait_for_first_connection_unusable(first_unusable, "HTTP/1.1").await;

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:http1-stale"));
    assert_stale_connection_observations(&observations, http::Version::HTTP_11);
}

#[tokio::test]
async fn forward_real_incoming_multipart_to_http11_upstream_uses_and_pools_fresh_connection() {
    let (upstream_addr, counter) =
        start_identified_http1_multipart_upstream("multipart-ok:http1-healthy").await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let upstream_origin = format!("http://{upstream_addr}");

    let warm = client
        .get(&format!("{upstream_origin}/warm"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.text().await.unwrap(), "warm:connection-1");
    assert_eq!(counter.connections(), 1);

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;
    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {body:?}");
    assert_eq!(
        body,
        Bytes::from_static(b"multipart-ok:http1-healthy:connection-2")
    );
    assert_eq!(
        counter.connections(),
        2,
        "the forwarded Incoming upload must bypass the healthy pooled H1.1 connection"
    );

    let follow_up = client
        .get(&format!("{upstream_origin}/follow-up"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(follow_up.text().await.unwrap(), "follow-up:connection-2");
    assert_eq!(
        counter.connections(),
        2,
        "the follow-up request must reuse the fresh upload connection"
    );
    assert_eq!(counter.requests(), 3);
}

#[tokio::test]
async fn forward_real_incoming_multipart_to_h2c_upstream() {
    let (upstream_addr, _counter) = aioduct_test_server::h2::h2_server_with(|req| {
        multipart_validation_response(req, "multipart-ok:h2c", http::Version::HTTP_2)
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let broker_addr = start_forwarding_broker_with_protocol(
        client,
        format!("http://{upstream_addr}")
            .parse::<http::Uri>()
            .unwrap(),
        ForwardProtocol::H2c,
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:h2c"));
}

#[tokio::test]
async fn forward_real_incoming_multipart_to_h2c_upstream_uses_and_pools_fresh_connection() {
    let (upstream_addr, counter) =
        start_identified_h2c_multipart_upstream("multipart-ok:h2c-healthy").await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let upstream_origin = format!("http://{upstream_addr}");

    let warm = client
        .get(&format!("{upstream_origin}/warm"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.version(), http::Version::HTTP_2);
    assert_eq!(warm.text().await.unwrap(), "warm:connection-1");
    assert_eq!(counter.connections(), 1);

    let broker_addr = start_forwarding_broker_with_protocol(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
        ForwardProtocol::H2c,
    )
    .await;
    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {body:?}");
    assert_eq!(
        body,
        Bytes::from_static(b"multipart-ok:h2c-healthy:connection-2")
    );
    assert_eq!(
        counter.connections(),
        2,
        "the forwarded Incoming upload must bypass the healthy pooled H2C connection"
    );

    let follow_up = client
        .get(&format!("{upstream_origin}/follow-up"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(follow_up.version(), http::Version::HTTP_2);
    assert_eq!(follow_up.text().await.unwrap(), "follow-up:connection-2");
    assert_eq!(
        counter.connections(),
        2,
        "the follow-up request must reuse the fresh H2C upload connection"
    );
    assert_eq!(counter.requests(), 3);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_real_incoming_multipart_to_https_http11_upstream() {
    aioduct_test_server::tls::install_crypto_provider();

    let (upstream_addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |req| {
            multipart_validation_response(req, "multipart-ok:https-h1", http::Version::HTTP_11)
        })
        .await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let broker_addr = start_forwarding_broker(
        client,
        format!("https://localhost:{}", upstream_addr.port())
            .parse::<http::Uri>()
            .unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:https-h1"));
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_real_incoming_multipart_to_https_h2_upstream() {
    aioduct_test_server::tls::install_crypto_provider();

    let (upstream_addr, cert_der, _counter) = aioduct_test_server::tls::tls_h2_server_with(|req| {
        multipart_validation_response(req, "multipart-ok:https", http::Version::HTTP_2)
    })
    .await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let broker_addr = start_forwarding_broker(
        client,
        format!("https://localhost:{}", upstream_addr.port())
            .parse::<http::Uri>()
            .unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:https"));
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_real_incoming_multipart_to_https_h2_upstream_uses_and_pools_fresh_connection() {
    aioduct_test_server::tls::install_crypto_provider();

    let (upstream_addr, cert_der, counter) =
        start_identified_tls_h2_multipart_upstream("multipart-ok:h2-healthy").await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let upstream_origin = format!("https://localhost:{}", upstream_addr.port());

    let warm = client
        .get(&format!("{upstream_origin}/warm"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.version(), http::Version::HTTP_2);
    assert_eq!(warm.text().await.unwrap(), "warm:connection-1");
    assert_eq!(counter.connections(), 1);

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;
    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {body:?}");
    assert_eq!(
        body,
        Bytes::from_static(b"multipart-ok:h2-healthy:connection-2")
    );
    assert_eq!(
        counter.connections(),
        2,
        "the forwarded Incoming upload must bypass the healthy pooled H2 connection"
    );

    let follow_up = client
        .get(&format!("{upstream_origin}/follow-up"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(follow_up.version(), http::Version::HTTP_2);
    assert_eq!(follow_up.text().await.unwrap(), "follow-up:connection-2");
    assert_eq!(
        counter.connections(),
        2,
        "the follow-up request must reuse the fresh upload connection"
    );
    assert_eq!(counter.requests(), 3);
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_real_incoming_multipart_to_https_h2_upstream_succeeds_after_goaway() {
    aioduct_test_server::tls::install_crypto_provider();

    let (upstream_addr, cert_der, observations, first_unusable) =
        start_tls_h2_goaway_multipart_upstream("multipart-ok:h2-goaway").await;
    let client_config = aioduct_test_server::tls::make_client_config(&cert_der);
    let connector = aioduct::tls::RustlsConnector::new(client_config);
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let upstream_origin = format!("https://localhost:{}", upstream_addr.port());

    let warm = client
        .get(&format!("{upstream_origin}/warm"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.text().await.unwrap(), "warm");
    wait_for_first_connection_unusable(first_unusable, "HTTPS/H2 GOAWAY").await;

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:h2-goaway"));
    assert_stale_connection_observations(&observations, http::Version::HTTP_2);
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_real_incoming_multipart_to_h3_upstream() {
    let (upstream_addr, _cert_der, _counter) =
        aioduct_test_server::h3::h3_server_with(|req, body| {
            multipart_validation_h3_response(req, body, "multipart-ok:h3")
        })
        .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let broker_addr = start_forwarding_broker(
        client,
        format!("https://127.0.0.1:{}", upstream_addr.port())
            .parse::<http::Uri>()
            .unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:h3"));
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_real_incoming_multipart_to_h3_upstream_succeeds_after_closed_pool_connection() {
    let (upstream_addr, _cert_der, observations, close_first, first_unusable) =
        start_h3_closed_after_warm_multipart_upstream("multipart-ok:h3-closed").await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let upstream_origin = format!("https://127.0.0.1:{}", upstream_addr.port());

    let warm = client
        .get(&format!("{upstream_origin}/warm"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.version(), http::Version::HTTP_3);
    assert_eq!(warm.text().await.unwrap(), "warm");
    close_first
        .send(())
        .expect("H3 server should still be waiting to close the warm connection");
    wait_for_first_connection_unusable(first_unusable, "HTTP/3").await;

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:h3-closed"));
    assert_stale_connection_observations(&observations, http::Version::HTTP_3);
}
