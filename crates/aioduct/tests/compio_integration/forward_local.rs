use super::*;

#[path = "forward_local/connect_validation.rs"]
mod connect_validation;

fn start_stale_multipart_server_with_tokio()
-> (SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let connections2 = connections.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let connection_index =
                    connections2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    if connection_index == 0 {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};

                        let mut buf = [0u8; 1024];
                        let _ = stream.read(&mut buf).await;
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
                            service_fn(|req: Request<hyper::body::Incoming>| async move {
                                let content_type = req
                                    .headers()
                                    .get(http::header::CONTENT_TYPE)
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or_default()
                                    .to_owned();
                                let body = req.into_body().collect().await.unwrap().to_bytes();
                                let ok = content_type.starts_with("multipart/form-data; boundary=")
                                    && body.windows(b"compio-file-bytes".len()).any(|window| {
                                        window == b"compio-file-bytes"
                                    });
                                let response = if ok {
                                    Response::new(Full::new(Bytes::from_static(b"fresh")))
                                } else {
                                    Response::builder()
                                        .status(http::StatusCode::BAD_REQUEST)
                                        .body(Full::new(Bytes::from_static(b"bad multipart")))
                                        .unwrap()
                                };
                                Ok::<_, Infallible>(response)
                            }),
                        )
                        .await;
                });
            }
        });
    });

    (rx.recv().unwrap(), connections)
}

fn start_h2_options_server_with_tokio() -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
                let authority = request
                    .uri()
                    .authority()
                    .map(http::uri::Authority::as_str)
                    .unwrap_or("missing");
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                    "method={};version={:?};scheme={};authority={authority};path={}",
                    request.method(),
                    request.version(),
                    request.uri().scheme_str().unwrap_or("missing"),
                    request.uri().path(),
                )))))
            })
            .await;
            tx.send(addr).unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

fn start_h2_trailer_server_with_tokio() -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    tokio::spawn(async move {
                        let io = aioduct_test_server::TokioIo::new(stream);
                        let _ = hyper::server::conn::http2::Builder::new(
                            aioduct_test_server::TokioExec,
                        )
                        .serve_connection(
                            io,
                            service_fn(|_request| async move {
                                let mut trailers = http::HeaderMap::new();
                                trailers.insert("x-result-checksum", "sum".parse().unwrap());
                                let frames = futures_util::stream::iter([
                                    Ok::<_, Infallible>(http_body::Frame::data(
                                        Bytes::from_static(b"data"),
                                    )),
                                    Ok(http_body::Frame::trailers(trailers)),
                                ]);
                                Ok::<_, Infallible>(
                                    Response::builder()
                                        .header(http::header::CONTENT_LENGTH, "4")
                                        .header(http::header::TRAILER, "x-result-checksum")
                                        .body(http_body_util::StreamBody::new(frames))
                                        .unwrap(),
                                )
                            }),
                        )
                        .await;
                    });
                }
            });
    });
    rx.recv().unwrap()
}

fn start_h2_bodyless_trailer_server_with_tokio(status: http::StatusCode) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    tokio::spawn(async move {
                        let io = aioduct_test_server::TokioIo::new(stream);
                        let _ = hyper::server::conn::http2::Builder::new(
                            aioduct_test_server::TokioExec,
                        )
                        .serve_connection(
                            io,
                            service_fn(move |_request| async move {
                                let mut trailers = http::HeaderMap::new();
                                trailers.insert("x-result", "must-reject".parse().unwrap());
                                let frames = futures_util::stream::iter([Ok::<_, Infallible>(
                                    http_body::Frame::<Bytes>::trailers(trailers),
                                )]);
                                Ok::<_, Infallible>(
                                    Response::builder()
                                        .status(status)
                                        .body(http_body_util::StreamBody::new(frames))
                                        .unwrap(),
                                )
                            }),
                        )
                        .await;
                    });
                }
            });
    });
    rx.recv().unwrap()
}

fn start_h2_forbidden_payload_server_with_tokio(status: http::StatusCode) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(stream).await.unwrap();
                let (_request, mut respond) = connection.accept().await.unwrap().unwrap();
                let mut body = respond
                    .send_response(Response::builder().status(status).body(()).unwrap(), false)
                    .unwrap();
                body.send_data(Bytes::from_static(b"forbidden"), true)
                    .unwrap();
                let _ = tokio::time::timeout(Duration::from_secs(1), connection.accept()).await;
            });
    });
    rx.recv().unwrap()
}

#[cfg(feature = "rustls")]
fn start_tls_h2_authority_server_with_tokio() -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    aioduct_test_server::ConnectionCounter,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let (addr, cert, counter) =
                aioduct_test_server::tls::tls_h2_server_with(|request| async move {
                    let authority = request
                        .uri()
                        .authority()
                        .map(http::uri::Authority::as_str)
                        .unwrap_or("missing");
                    let host = request
                        .headers()
                        .get(http::header::HOST)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("missing");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "authority={authority};host={host}"
                    )))))
                })
                .await;
            tx.send((addr, cert, counter)).unwrap();
            std::future::pending::<()>().await;
        });
    });
    rx.recv().unwrap()
}

#[cfg(feature = "rustls")]
fn start_tls_options_server_with_tokio() -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    aioduct_test_server::ConnectionCounter,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let (addr, cert, counter) = aioduct_test_server::tls::tls_server_with(
                    &[b"h2", b"http/1.1"],
                    |request| async move {
                        let authority = request
                            .uri()
                            .authority()
                            .map(http::uri::Authority::as_str)
                            .unwrap_or("missing");
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                            "version={:?};authority={authority};path={}",
                            request.version(),
                            request.uri().path()
                        )))))
                    },
                )
                .await;
                tx.send((addr, cert, counter)).unwrap();
                std::future::pending::<()>().await;
            });
    });
    rx.recv().unwrap()
}

// ── Forward: on_request hook ──────────────────────────────────────────

#[test]
fn test_compio_forward_h2c_options_asterisk() {
    let upstream_addr = start_h2_options_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let request = http::Request::builder()
            .method(http::Method::OPTIONS)
            .uri("*")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = HttpEngineLocal::<CompioRuntime, TcpConnector>::new()
            .forward_local(valid_forward_request(request))
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            // h2's server-side URI cannot retain `:scheme` without an
            // authority. The missing URI authority proves that the literal
            // asterisk-form target did not gain `:authority` while forwarding.
            "method=OPTIONS;version=HTTP/2.0;scheme=missing;authority=missing;path=*"
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_forward_local_https_options_uses_h1_and_rejects_exact_h2() {
    let (addr, cert, counter) = start_tls_options_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector =
            aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
        let client: HttpEngineLocal<CompioRuntime, TcpConnector> = HttpEngineLocal::builder()
            .tls(connector)
            .build_local()
            .unwrap();
        let request = http::Request::builder()
            .method(http::Method::OPTIONS)
            .uri("*")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "downstream.test")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = client
            .forward_local(request)
            .upstream(format!("https://localhost:{}", addr.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.text().await.unwrap(),
            "version=HTTP/1.1;authority=missing;path=*"
        );

        let request = http::Request::builder()
            .method(http::Method::OPTIONS)
            .uri("*")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "downstream.test")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let error = client
            .forward_local(request)
            .upstream(format!("https://localhost:{}", addr.port()))
            .on_request(|parts| parts.version = http::Version::HTTP_2)
            .send()
            .await
            .unwrap_err();
        assert!(
            matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("cannot be represented by the HTTP/2 transport")),
            "{error}"
        );
    });

    assert_eq!(counter.requests(), 1);
}

#[test]
fn test_compio_forward_h2c_options_signature_uses_asterisk_semantics() {
    let upstream_addr = start_h2_options_server_with_tokio();
    let bases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let signer_bases = bases.clone();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let config = MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::target_uri())
            .component(MessageSignatureComponent::request_target());
        let signer = move |base: &[u8]| -> Result<Vec<u8>, aioduct::MessageSignatureError> {
            signer_bases
                .lock()
                .unwrap()
                .push(std::str::from_utf8(base).unwrap().to_owned());
            Ok(b"compio-options".to_vec())
        };
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .message_signature(config, signer)
            .build_local()
            .unwrap();
        let request = http::Request::builder()
            .method(http::Method::OPTIONS)
            .uri("*")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = client
            .forward_local(valid_forward_request(request))
            .upstream(
                format!("http://{upstream_addr}/proxy/base")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::OK);
    });

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(&format!(r#""@target-uri": http://{upstream_addr}"#)),
        "{}",
        bases[0]
    );
    assert!(bases[0].contains(r#""@request-target": *"#), "{}", bases[0]);
    assert!(!bases[0].contains("/proxy/base"), "{}", bases[0]);
}

#[test]
fn test_compio_forward_on_request_hook() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let custom = req
            .headers()
            .get("x-injected")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_request(|parts| {
                parts.headers.insert(
                    "x-injected",
                    http::header::HeaderValue::from_static("hook-value"),
                );
            })
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hook-value");
    });
}

#[test]
fn test_compio_forward_finalizes_hook_uri_and_ingress_version() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{:?} {}",
            req.version(),
            req.uri()
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/ingress")
            .version(http::Version::HTTP_3)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}/base", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_request(|parts| {
                parts.uri = "/hooked?q=local".parse().unwrap();
            })
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "HTTP/1.1 /hooked?q=local");
    });
}

#[test]
fn test_compio_forward_rejects_exact_http3_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/ingress")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .on_request(|parts| parts.version = http::Version::HTTP_3)
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::Unsupported(_)));
    });

    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn test_compio_forward_local_rejects_connect_host_without_port_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let request = http::Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "target.example")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let error = HttpEngineLocal::<CompioRuntime, TcpConnector>::new()
            .forward_local(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();
        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    });

    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn test_compio_forward_uses_shared_header_sanitizer() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let request_clean = !req.headers().contains_key("connection")
            && !req.headers().contains_key("x-first-hop")
            && !req.headers().contains_key("x-second-hop")
            && !req.headers().contains_key("upgrade")
            && !req.headers().contains_key("te");
        let mut response = Response::new(Full::new(Bytes::from(request_clean.to_string())));
        response.headers_mut().append(
            "connection",
            http::header::HeaderValue::from_static("x-upstream-hop"),
        );
        response.headers_mut().insert(
            "x-upstream-hop",
            http::header::HeaderValue::from_static("secret"),
        );
        Ok::<_, Infallible>(response)
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let mut incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();
        incoming.headers_mut().append(
            "connection",
            http::header::HeaderValue::from_static("X-First-Hop"),
        );
        incoming.headers_mut().append(
            "connection",
            http::header::HeaderValue::from_static("x-SECOND-hop, bad token"),
        );
        incoming.headers_mut().insert(
            "x-first-hop",
            http::header::HeaderValue::from_static("secret-1"),
        );
        incoming.headers_mut().insert(
            "x-second-hop",
            http::header::HeaderValue::from_static("secret-2"),
        );
        incoming
            .headers_mut()
            .insert("upgrade", http::header::HeaderValue::from_static("h2c"));
        incoming
            .headers_mut()
            .insert("te", http::header::HeaderValue::from_static("trailers"));

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_response(|response| {
                response.headers_mut().append(
                    "connection",
                    http::header::HeaderValue::from_static("x-hook-hop"),
                );
                response.headers_mut().insert(
                    "x-hook-hop",
                    http::header::HeaderValue::from_static("secret"),
                );
            })
            .send()
            .await
            .unwrap();

        assert!(!resp.headers().contains_key("connection"));
        assert!(!resp.headers().contains_key("x-upstream-hop"));
        assert!(!resp.headers().contains_key("x-hook-hop"));
        assert_eq!(resp.text().await.unwrap(), "true");
    });
}

#[test]
fn test_compio_forward_local_restores_canonical_host_after_sanitization() {
    let addr = start_server_with_tokio(|request| async move {
        let host = request
            .headers()
            .get(http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        let connection = request
            .headers()
            .get(http::header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "host={host};connection={connection}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri("/chat")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "broker.test")
            .header(http::header::CONNECTION, "upgrade, host")
            .header(http::header::UPGRADE, "websocket")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = HttpEngineLocal::<CompioRuntime, TcpConnector>::new()
            .forward_local(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            format!("host={addr};connection=upgrade")
        );
    });
}

#[test]
fn test_compio_forward_strips_forbidden_trailer_fields() {
    let upstream_addr = start_server_with_tokio(|request| async move {
        let negotiated_trailers = request
            .headers()
            .get(http::header::TE)
            .is_some_and(|value| value == "trailers")
            && request
                .headers()
                .get(http::header::CONNECTION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .any(|token| token.eq_ignore_ascii_case("te"))
                });
        let collected = request.into_body().collect().await.unwrap();
        let trailers = collected.trailers().unwrap();
        let clean = negotiated_trailers
            && trailers
                .get("x-upload-checksum")
                .is_some_and(|v| v == "local")
            && !trailers.contains_key(http::header::AUTHORIZATION)
            && !trailers.contains_key(http::header::CONTENT_TYPE)
            && !trailers.contains_key(http::header::CONNECTION)
            && !trailers.contains_key("x-hop-secret");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(clean.to_string()))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-upload-checksum", "local".parse().unwrap());
        trailers.insert(
            http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        trailers.insert(http::header::CONTENT_TYPE, "text/plain".parse().unwrap());
        trailers.insert(http::header::CONNECTION, "x-hop-secret".parse().unwrap());
        trailers.insert("x-hop-secret", "secret".parse().unwrap());
        let body = http_body_util::StreamBody::new(futures_util::stream::iter([
            Ok::<_, Infallible>(http_body::Frame::data(Bytes::from_static(b"body"))),
            Ok(http_body::Frame::trailers(trailers)),
        ]));
        let incoming = http::Request::builder()
            .method("POST")
            .uri("/upload")
            .header(
                http::header::TRAILER,
                "x-upload-checksum, Authorization, Content-Type",
            )
            .header(http::header::CONNECTION, "TE")
            .header(http::header::TE, "trailers")
            .body(body)
            .unwrap();

        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "true");
    });
}

#[test]
fn test_compio_forward_automatic_message_signature() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let signature_input = req
            .headers()
            .get("signature-input")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        let signature = req
            .headers()
            .get("signature")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{signature_input}\n{signature}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let config = MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::method())
            .component(MessageSignatureComponent::request_target());
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .message_signature(config, compio_signature)
            .build_local()
            .unwrap();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/forwarded")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.contains("sig1="), "{body}");
        assert!(body.contains("sig1=:Y29tcGlv:"), "{body}");
    });
}

#[test]
fn test_compio_forward_local_non_replayable_multipart_skips_stale_pool() {
    let (upstream_addr, counter) = start_stale_multipart_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .build_local()
            .unwrap();
        let upstream = format!("http://127.0.0.1:{}", upstream_addr.port());

        let warm = client
            .get_local(&format!("{upstream}/warm"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(warm.text().await.unwrap(), "warm");
        std::thread::sleep(Duration::from_millis(75));

        let body = Bytes::from_static(
            b"--compioBoundary\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"upload.bin\"\r\n\
Content-Type: application/octet-stream\r\n\r\n\
compio-file-bytes\r\n\
--compioBoundary--\r\n",
        );
        let incoming = http::Request::builder()
            .method("POST")
            .uri("/upload")
            .header(
                http::header::CONTENT_TYPE,
                "multipart/form-data; boundary=compioBoundary",
            )
            .body(Full::new(body))
            .unwrap();

        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(upstream.parse::<http::Uri>().unwrap())
            .send()
            .await
            .expect("a forwarded one-shot body should start on a fresh connection");

        assert_eq!(response.text().await.unwrap(), "fresh");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the forwarded one-shot body must bypass the stale pooled connection"
        );
    });
}

// ── Forward: on_response hook ─────────────────────────────────────────

#[test]
fn test_compio_forward_on_response_cannot_promote_failed_connect_to_success() {
    let upstream_addr = start_server_with_tokio(|request| async move {
        let status = if request.method() == http::Method::CONNECT {
            http::StatusCode::PROXY_AUTHENTICATION_REQUIRED
        } else {
            assert_eq!(request.uri().path(), "/replacement");
            http::StatusCode::OK
        };
        Ok::<_, Infallible>(
            Response::builder()
                .status(status)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });
    let replacement_runtime = tokio::runtime::Runtime::new().unwrap();
    let replacement = replacement_runtime.block_on(async {
        aioduct::HttpEngineSend::<
            aioduct::runtime::TokioRuntime,
            aioduct::runtime::tokio_rt::TcpConnector,
        >::new()
        .get(&format!("http://{upstream_addr}/replacement"))
        .unwrap()
        .send()
        .await
        .unwrap()
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let request = http::Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "target.example:443")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = HttpEngineLocal::<CompioRuntime, TcpConnector>::new()
            .forward_local(request)
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_response(move |response| *response = replacement)
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, aioduct::Error::InvalidHeader(ref message) if message.contains("establishes a tunnel")),
            "{error}"
        );
    });
}

#[test]
fn test_compio_forward_on_response_hook() {
    let upstream_addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("original"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_response(|resp| {
                resp.headers_mut().insert(
                    "x-modified",
                    http::header::HeaderValue::from_static("by-hook"),
                );
            })
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("x-modified").unwrap().to_str().unwrap(),
            "by-hook"
        );
    });
}

#[test]
fn test_compio_forward_response_message_signature() {
    let upstream_addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(200)
                .header("connection", "x-upstream-hop")
                .header("x-upstream-hop", "remove-me")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let bases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let signer_bases = bases.clone();
        let signer = move |base: &[u8]| -> Result<Vec<u8>, aioduct::MessageSignatureError> {
            signer_bases
                .lock()
                .unwrap()
                .push(std::str::from_utf8(base).unwrap().to_owned());
            Ok(b"compio-response".to_vec())
        };
        let config = MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::status())
            .component(MessageSignatureComponent::header(
                http::header::HeaderName::from_static("x-local"),
            ));
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_response(|resp| {
                resp.headers_mut()
                    .insert("x-local", http::header::HeaderValue::from_static("hooked"));
            })
            .response_message_signature(config, signer)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(
            resp.headers().get("signature").unwrap().to_str().unwrap(),
            "sig1=:Y29tcGlvLXJlc3BvbnNl:"
        );
        assert!(!resp.headers().contains_key("connection"));
        assert!(!resp.headers().contains_key("x-upstream-hop"));

        let bases = bases.lock().unwrap();
        assert_eq!(bases.len(), 1);
        assert!(bases[0].contains(r#""@status": 200"#));
        assert!(bases[0].contains(r#""x-local": hooked"#));
    });
}

#[test]
fn test_compio_forward_response_content_digest_is_signed() {
    let upstream_addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("local signed body"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let bases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let signer_bases = bases.clone();
        let signer = move |base: &[u8]| -> Result<Vec<u8>, aioduct::MessageSignatureError> {
            signer_bases
                .lock()
                .unwrap()
                .push(std::str::from_utf8(base).unwrap().to_owned());
            Ok(b"local-digest".to_vec())
        };
        let config = MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::status())
            .component(MessageSignatureComponent::header(
                http::header::HeaderName::from_static(CONTENT_DIGEST),
            ));
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/digest")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .response_content_digest(1024)
            .response_message_signature(config, signer)
            .send()
            .await
            .unwrap();

        let expected_digest = sha256_content_digest_value(b"local signed body").unwrap();
        assert_eq!(
            resp.headers().get(CONTENT_DIGEST).unwrap(),
            &expected_digest
        );
        assert_eq!(
            resp.headers().get("signature").unwrap().to_str().unwrap(),
            "sig1=:bG9jYWwtZGlnZXN0:"
        );
        assert_eq!(resp.text().await.unwrap(), "local signed body");

        let bases = bases.lock().unwrap();
        assert_eq!(bases.len(), 1);
        assert!(bases[0].contains(&format!(
            r#""content-digest": {}"#,
            expected_digest.to_str().unwrap()
        )));
    });
}

#[test]
fn test_compio_forward_response_content_digest_skips_not_modified_response() {
    let upstream_addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(http::StatusCode::NOT_MODIFIED)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/cached")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .response_content_digest(0)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::NOT_MODIFIED);
        assert!(!resp.headers().contains_key(CONTENT_DIGEST));
    });
}

#[test]
fn test_compio_forward_response_async_signing_is_included_in_timeout() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    let upstream_addr = start_server_with_tokio(move |_req| {
        let server_attempts = server_attempts.clone();
        async move {
            server_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let config = MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::status());
        let signer = |_base: MessageSignatureBase| async move {
            std::future::pending::<()>().await;
            Ok::<_, aioduct::MessageSignatureError>(b"late".to_vec())
        };
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/slow-response-sign")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .response_message_signature_async_local(config, signer)
            .timeout(Duration::from_millis(20))
            .send()
            .await;

        assert!(result.unwrap_err().is_timeout());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

// ── Forward: remove_header ────────────────────────────────────────────

#[test]
fn test_compio_forward_remove_header() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let has_secret = req.headers().contains_key("x-secret");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "secret={}",
            has_secret
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .header("x-secret", "confidential")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .remove_header(http::header::HeaderName::from_static("x-secret"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "secret=false");
    });
}

// ── Forward: forward_header ───────────────────────────────────────────

#[test]
fn test_compio_forward_forward_header() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let auth = req
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_else(|| "missing".to_owned());
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(auth))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .header("authorization", "Bearer my-token")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .forward_header(http::header::AUTHORIZATION)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "Bearer my-token");
    });
}

#[test]
fn test_compio_forward_upgrade_field_without_connection_upgrade_token_strips_upgrade_fields() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let has_connection = req.headers().contains_key("connection");
        let has_upgrade = req.headers().contains_key("upgrade");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "conn={},upgrade={}",
            has_connection, has_upgrade
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/h2c-probe")
            .header("connection", "keep-alive")
            .header("upgrade", "h2c")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.text().await.unwrap(), "conn=false,upgrade=false");
    });
}

// ── Forward: timeout ──────────────────────────────────────────────────

#[test]
fn test_compio_forward_timeout() {
    let upstream_addr = start_server_with_tokio(|_req| async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("slow"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .timeout(Duration::from_millis(50))
            .send()
            .await;

        assert!(
            result.is_err(),
            "forward with timeout should error on slow upstream"
        );
        assert!(result.unwrap_err().is_timeout());
    });
}

#[test]
fn test_compio_forward_async_signing_is_included_in_timeout() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    let upstream_addr = start_server_with_tokio(move |_req| {
        let server_attempts = server_attempts.clone();
        async move {
            server_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("unexpected"))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let config = MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::method())
            .component(MessageSignatureComponent::request_target());
        let signer = |_base: MessageSignatureBase| async move {
            std::future::pending::<()>().await;
            Ok::<_, aioduct::MessageSignatureError>(b"late".to_vec())
        };
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .message_signature_async_local(config, signer)
            .build_local()
            .unwrap();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/slow-sign")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .timeout(Duration::from_millis(20))
            .send()
            .await;

        assert!(result.unwrap_err().is_timeout());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 0);
    });
}

// ── Forward: upstream with base path ──────────────────────────────────

#[test]
fn test_compio_forward_upstream_base_path() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_owned();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/resource/123")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}/api/v1", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "/api/v1/resource/123");
    });
}

// ── Forward: query string preserved ──────────────────────────────────

#[test]
fn test_compio_forward_query_string_preserved() {
    let upstream_addr = start_server_with_tokio(|req| async move {
        let full = req.uri().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(full))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/search?q=hello&page=1")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("q=hello"),
            "query should be preserved: {body}"
        );
        assert!(body.contains("page=1"), "query should be preserved: {body}");
    });
}

// ── Forward: no upstream returns error ────────────────────────────────

#[test]
fn test_compio_forward_no_upstream_errors() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client
            .forward_local(super::valid_forward_request(incoming))
            .send()
            .await;
        assert!(result.is_err(), "forward without upstream should fail");
    });
}

// ── Forward local: preserve_host and upstream base path ───────────────

#[test]
fn test_compio_forward_local_preserve_host_with_base_path() {
    let addr = start_server_with_tokio(|req| async move {
        let host = req
            .headers()
            .get("host")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "host={host},path={path}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/users/123")
            .header("host", "original.example.com")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{addr}/api/v2"))
            .preserve_host()
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(
            body.contains("host=original.example.com"),
            "preserve_host should keep original, got: {body}"
        );
        assert!(
            body.contains("path=/api/v2/users/123"),
            "base path should be prepended, got: {body}"
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_forward_local_preserve_host_sets_negotiated_h2_authority() {
    const ORIGINAL_AUTHORITY: &str = "local-original.example:8443";
    let (addr, cert, _counter) = start_tls_h2_authority_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector =
            aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
        let client: HttpEngineLocal<CompioRuntime, TcpConnector> = HttpEngineLocal::builder()
            .tls(connector)
            .build_local()
            .unwrap();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/resource")
            .header(http::header::HOST, ORIGINAL_AUTHORITY)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("https://localhost:{}", addr.port()))
            .preserve_host()
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            format!("authority={ORIGINAL_AUTHORITY};host={ORIGINAL_AUTHORITY}")
        );
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_forward_local_rejects_invalid_te_after_h2_negotiation() {
    let (addr, cert, counter) = start_tls_h2_authority_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector =
            aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
        let client: HttpEngineLocal<CompioRuntime, TcpConnector> = HttpEngineLocal::builder()
            .tls(connector)
            .build_local()
            .unwrap();
        let incoming = http::Request::builder()
            .uri("/resource")
            .header(http::header::TE, "gzip")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("https://localhost:{}", addr.port()))
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidHeader(_)));
        assert_eq!(counter.requests(), 0);
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_forward_local_rejects_invalid_header_value_after_h2_negotiation() {
    let (addr, cert, counter) = start_tls_h2_authority_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector =
            aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
        let client: HttpEngineLocal<CompioRuntime, TcpConnector> = HttpEngineLocal::builder()
            .tls(connector)
            .build_local()
            .unwrap();
        let incoming = http::Request::builder()
            .uri("/resource")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("https://localhost:{}", addr.port()))
            .on_request(|parts| {
                parts.headers.insert(
                    "x-invalid",
                    http::HeaderValue::from_bytes(b" leading-space").unwrap(),
                );
            })
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
        assert_eq!(counter.requests(), 0);
    });
}

#[cfg(feature = "rustls")]
#[test]
fn test_compio_forward_local_rejects_post_hook_content_length_mismatch() {
    let (addr, cert, counter) = start_tls_h2_authority_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let connector =
            aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
        let client: HttpEngineLocal<CompioRuntime, TcpConnector> = HttpEngineLocal::builder()
            .tls(connector)
            .build_local()
            .unwrap();
        let incoming = http::Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .body(Full::new(Bytes::from_static(b"data")))
            .unwrap();

        let error = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("https://localhost:{}", addr.port()))
            .on_request(|parts| {
                parts
                    .headers
                    .insert(http::header::CONTENT_LENGTH, "3".parse().unwrap());
            })
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
        assert_eq!(counter.requests(), 0);
    });
}

#[test]
fn test_compio_forward_local_translates_h2_trailers_for_http10() {
    let addr = start_h2_trailer_server_with_tokio();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method(http::Method::GET)
            .uri("/result")
            .version(http::Version::HTTP_10)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .forward_local(incoming)
            .upstream(format!("http://{addr}"))
            .h2c()
            .send()
            .await
            .unwrap();

        assert_eq!(response.version(), http::Version::HTTP_10);
        assert_eq!(response.headers()[http::header::CONTENT_LENGTH], "4");
        assert!(
            !response
                .headers()
                .contains_key(http::header::TRANSFER_ENCODING)
        );
        assert!(!response.headers().contains_key(http::header::TRAILER));
        let collected = response
            .into_http_response()
            .into_body()
            .collect()
            .await
            .unwrap();
        assert!(collected.trailers().is_none());
        assert_eq!(collected.to_bytes(), Bytes::from_static(b"data"));
    });
}

#[test]
fn test_compio_forward_local_rejects_h2_204_and_304_trailers_before_handoff() {
    for status in [http::StatusCode::NO_CONTENT, http::StatusCode::NOT_MODIFIED] {
        let addr = start_h2_bodyless_trailer_server_with_tokio(status);
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
            let incoming = http::Request::builder()
                .method(http::Method::GET)
                .uri("/bodyless-trailers")
                .version(http::Version::HTTP_11)
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap();

            let error = client
                .forward_local(incoming)
                .upstream(format!("http://{addr}"))
                .h2c()
                .send()
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("must not contain trailers"),
                "unexpected {status} error: {error}"
            );
        });
    }
}

#[test]
fn test_compio_forward_local_rejects_h2_head_and_205_payloads_before_handoff() {
    for (method, status) in [
        (http::Method::HEAD, http::StatusCode::OK),
        (http::Method::GET, http::StatusCode::RESET_CONTENT),
    ] {
        let addr = start_h2_forbidden_payload_server_with_tokio(status);
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let request = http::Request::builder()
                .method(method.clone())
                .uri("/forbidden-payload")
                .version(http::Version::HTTP_11)
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let error = HttpEngineLocal::<CompioRuntime, TcpConnector>::new()
                .forward_local(request)
                .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
                .h2c()
                .send()
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    aioduct::Error::Hyper(_) | aioduct::Error::InvalidHeader(_)
                ),
                "unexpected Local {method} {status} error: {error}"
            );
        });
    }
}

// ── Forward local: strip prefix ───────────────────────────────────────

#[test]
fn test_compio_forward_local_strip_prefix() {
    let addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "path={path}"
        )))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let incoming = http::Request::builder()
            .method("GET")
            .uri("/api/users/456")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let resp = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{addr}"))
            .strip_prefix("/api")
            .send()
            .await
            .unwrap();

        let body = resp.text().await.unwrap();
        assert!(
            body.contains("path=/users/456"),
            "strip_prefix should remove /api, got: {body}"
        );
    });
}

// ── Forward local: client timeout fires ───────────────────────────────

#[test]
fn test_compio_forward_local_client_timeout() {
    // Start a server that never responds
    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();
            // Accept one connection but never respond
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .timeout(Duration::from_millis(100))
            .build_local()
            .unwrap();

        let incoming = http::Request::builder()
            .method("GET")
            .uri("/test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let result = client
            .forward_local(super::valid_forward_request(incoming))
            .upstream(format!("http://{addr}"))
            .send()
            .await;
        assert!(result.is_err(), "client timeout should fire for forward");
    });
}

// ── Cache store in finalize_response_local ────────────────────────────

#[test]
fn test_compio_finalize_response_local_caches() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let addr = start_server_with_tokio(move |_req| {
        let count = hit_count_clone.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from("cached local")))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let cache = aioduct::HttpCache::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cache(cache)
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/resource"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "cached local");
        assert_eq!(hit_count.load(Ordering::SeqCst), 1);

        // Second request should be from cache
        let resp = client
            .get_local(&format!("http://{addr}/resource"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "cached local");
        assert_eq!(hit_count.load(Ordering::SeqCst), 1, "should be from cache");
    });
}
