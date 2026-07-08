use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
#[cfg(feature = "rustls")]
use aioduct_test_server::TokioExec;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1 as server_http1;
#[cfg(feature = "rustls")]
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

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn validate_multipart_parts(
    method: &http::Method,
    uri: &http::Uri,
    headers: &http::HeaderMap,
    body: &[u8],
    expected_path: &str,
) -> Result<(), String> {
    if method != http::Method::POST {
        return Err(format!("expected POST, got {method}"));
    }
    if uri.path() != expected_path {
        return Err(format!("expected path {expected_path}, got {uri}"));
    }
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "missing content-type".to_owned())?
        .to_owned();
    if !content_type.starts_with("multipart/form-data; boundary=") {
        return Err(format!("unexpected content-type: {content_type}"));
    }

    for expected in [
        b"name=\"model\"".as_slice(),
        b"ocr-model".as_slice(),
        b"name=\"optionalPayload\"".as_slice(),
        b"name=\"file\"; filename=\"ocr-page.pdf\"".as_slice(),
        MULTIPART_FILE_BYTES,
    ] {
        if !bytes_contain(body, expected) {
            return Err(format!(
                "multipart body missing {}",
                String::from_utf8_lossy(expected)
            ));
        }
    }
    Ok(())
}

async fn validate_forwarded_multipart(
    req: Request<hyper::body::Incoming>,
    expected_path: &str,
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
        &parts.headers,
        &body,
        expected_path,
    )
}

async fn multipart_validation_response(
    req: Request<hyper::body::Incoming>,
    marker: &'static str,
) -> Result<Response<Full<Bytes>>, Infallible> {
    match validate_forwarded_multipart(req, "/api/v2/ocr/jobs").await {
        Ok(()) => Ok(Response::new(Full::new(Bytes::from_static(
            marker.as_bytes(),
        )))),
        Err(error) => Ok(Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(error)))
            .unwrap()),
    }
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
                        service_fn(move |req| multipart_validation_response(req, marker)),
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
) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let connections2 = connections.clone();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let connection_index = connections2.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if connection_index == 0 {
                    let _ = read_raw_headers(&mut stream).await;
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\nwarm",
                        )
                        .await
                        .unwrap();
                    stream.flush().await.unwrap();
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                    return;
                }

                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| multipart_validation_response(req, marker)),
                    )
                    .await;
            });
        }
    });

    (addr, connections)
}

#[cfg(feature = "rustls")]
async fn start_tls_h2_goaway_multipart_upstream(
    marker: &'static str,
) -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    Arc<AtomicUsize>,
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
    let connections = Arc::new(AtomicUsize::new(0));
    let connections2 = connections.clone();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let connection_index = connections2.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(stream) => stream,
                    Err(_) => return,
                };
                let io = aioduct::runtime::tokio_rt::TokioIo::new(tls_stream);

                if connection_index == 0 {
                    let request_count = Arc::new(AtomicUsize::new(0));
                    let request_count2 = request_count.clone();
                    let conn = server_http2::Builder::new(TokioExec).serve_connection(
                        io,
                        service_fn(move |req: Request<hyper::body::Incoming>| {
                            request_count2.fetch_add(1, Ordering::SeqCst);
                            async move {
                                if req.method() == http::Method::GET {
                                    Ok::<_, Infallible>(Response::new(Full::new(
                                        Bytes::from_static(b"warm"),
                                    )))
                                } else {
                                    multipart_validation_response(req, marker).await
                                }
                            }
                        }),
                    );
                    tokio::pin!(conn);
                    loop {
                        tokio::select! {
                            result = &mut conn => {
                                let _ = result;
                                break;
                            }
                            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                                if request_count.load(Ordering::SeqCst) >= 1 {
                                    conn.as_mut().graceful_shutdown();
                                }
                            }
                        }
                    }
                    return;
                }

                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |req| multipart_validation_response(req, marker)),
                    )
                    .await;
            });
        }
    });

    (addr, cert_der, connections)
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
        req.headers(),
        &body,
        "/api/v2/ocr/jobs",
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
    Arc<AtomicUsize>,
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
    let connections = Arc::new(AtomicUsize::new(0));
    let connections2 = connections.clone();

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let connection_index = connections2.fetch_add(1, Ordering::SeqCst);
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

                if connection_index == 0 {
                    let resolver = match h3_conn.accept().await {
                        Ok(Some(resolver)) => resolver,
                        _ => return,
                    };
                    let (_req, mut stream) = match resolver.resolve_request().await {
                        Ok(resolved) => resolved,
                        Err(_) => return,
                    };
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
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    close_conn.close(h3_quinn::quinn::VarInt::from_u32(0), b"warm complete");
                    return;
                }

                loop {
                    let resolver = match h3_conn.accept().await {
                        Ok(Some(resolver)) => resolver,
                        Ok(None) | Err(_) => break,
                    };
                    tokio::spawn(async move {
                        let (req, mut stream) = match resolver.resolve_request().await {
                            Ok(resolved) => resolved,
                            Err(_) => return,
                        };

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

    (addr, cert_der, connections)
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
async fn forward_real_incoming_multipart_to_http1_upstream_skips_closed_pool() {
    let (upstream_addr, counter) =
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
    tokio::time::sleep(Duration::from_millis(75)).await;

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:http1-stale"));
    assert!(
        counter.load(Ordering::SeqCst) >= 2,
        "non-replayable forwarded bodies should skip the closed pooled h1 connection"
    );
}

#[tokio::test]
async fn forward_real_incoming_multipart_to_h2c_upstream() {
    let (upstream_addr, _counter) = aioduct_test_server::h2::h2_server_with(|req| {
        multipart_validation_response(req, "multipart-ok:h2c")
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

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_real_incoming_multipart_to_https_http11_upstream() {
    aioduct_test_server::tls::install_crypto_provider();

    let (upstream_addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |req| {
            multipart_validation_response(req, "multipart-ok:https-h1")
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
        multipart_validation_response(req, "multipart-ok:https")
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
async fn forward_real_incoming_multipart_to_https_h2_upstream_skips_goaway_pool() {
    aioduct_test_server::tls::install_crypto_provider();

    let (upstream_addr, cert_der, counter) =
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
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert!(
        counter.load(Ordering::SeqCst) >= 1,
        "warm request should establish the upstream connection"
    );

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:h2-goaway"));
    assert!(
        counter.load(Ordering::SeqCst) >= 2,
        "non-replayable forwarded bodies should skip the GOAWAY'd h2 connection"
    );
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
async fn forward_real_incoming_multipart_to_h3_upstream_skips_closed_pool() {
    let (upstream_addr, _cert_der, counter) =
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

    let broker_addr = start_forwarding_broker(
        client.clone(),
        upstream_origin.parse::<http::Uri>().unwrap(),
    )
    .await;

    let (status, body) = post_raw_multipart(broker_addr).await;

    assert_eq!(status, 200, "broker returned body: {:?}", body);
    assert_eq!(body, Bytes::from_static(b"multipart-ok:h3-closed"));
    assert!(
        counter.load(Ordering::SeqCst) >= 2,
        "non-replayable forwarded bodies should skip the closed h3 connection"
    );
}
