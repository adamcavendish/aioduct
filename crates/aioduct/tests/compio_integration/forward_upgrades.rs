use super::*;

fn start_reusable_upgrade_server() -> (SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_connections = connections.clone();

    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    server_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::spawn(async move {
                        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                        let connection = server_http1::Builder::new()
                            .serve_connection(
                                io,
                                service_fn(
                                    |mut request: Request<hyper::body::Incoming>| async move {
                                        if request.uri().path() == "/warm" {
                                            return Ok::<_, Infallible>(Response::new(Full::new(
                                                Bytes::from_static(b"warm"),
                                            )));
                                        }

                                        tokio::spawn(async move {
                                            if let Ok(upgraded) =
                                                hyper::upgrade::on(&mut request).await
                                            {
                                                use tokio::io::{
                                                    AsyncReadExt as _, AsyncWriteExt as _,
                                                };
                                                let mut upgraded =
                                                    aioduct::UpgradedSend::from(upgraded);
                                                let mut buffer = [0; 64];
                                                if let Ok(size) = upgraded.read(&mut buffer).await {
                                                    let _ =
                                                        upgraded.write_all(&buffer[..size]).await;
                                                }
                                            }
                                        });
                                        Ok(Response::builder()
                                            .status(http::StatusCode::SWITCHING_PROTOCOLS)
                                            .header(http::header::CONNECTION, "upgrade")
                                            .header(http::header::UPGRADE, "test")
                                            .body(Full::new(Bytes::new()))
                                            .unwrap())
                                    },
                                ),
                            )
                            .with_upgrades();
                        let _ = connection.await;
                    });
                }
            });
    });

    (rx.recv().unwrap(), connections)
}

async fn h2_connect_echo_response(
    mut request: Request<hyper::body::Incoming>,
    expected_protocol: Option<&'static str>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.method() != http::Method::CONNECT {
        return Ok(Response::new(Full::new(Bytes::from_static(b"ordinary"))));
    }

    assert_eq!(
        request
            .extensions()
            .get::<hyper::ext::Protocol>()
            .map(hyper::ext::Protocol::as_str),
        expected_protocol
    );
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        if let Ok(upgraded) = hyper::upgrade::on(&mut request).await {
            let mut upgraded = aioduct::UpgradedSend::from(upgraded);
            let mut buffer = [0_u8; 64];
            loop {
                let read = match upgraded.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => read,
                };
                if upgraded.write_all(&buffer[..read]).await.is_err() {
                    return;
                }
            }
        }
    });
    Ok(Response::new(Full::new(Bytes::new())))
}

#[test]
fn local_forward_rejects_invalid_or_hook_mutated_connect_protocol_before_io() {
    #[derive(Clone, Copy)]
    enum Case {
        Create,
        Remove,
        Change,
        Invalid,
    }

    for case in [Case::Create, Case::Remove, Case::Change, Case::Invalid] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();

        compio_runtime::Runtime::new().unwrap().block_on(async {
            let mut request = Request::builder()
                .method(http::Method::CONNECT)
                .uri(if matches!(case, Case::Create) {
                    "downstream.test:443"
                } else {
                    "http://downstream.test/tunnel"
                })
                .version(http::Version::HTTP_2)
                .body(Full::new(Bytes::new()))
                .unwrap();
            if !matches!(case, Case::Create) {
                request
                    .extensions_mut()
                    .insert(if matches!(case, Case::Invalid) {
                        aioduct::Protocol::from_static("two words")
                    } else {
                        aioduct::Protocol::from_static("websocket")
                    });
            }

            let error = HttpEngineLocal::<CompioRuntime, TcpConnector>::new()
                .forward_local(request)
                .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
                .h2c()
                .on_request(move |parts| match case {
                    Case::Create => {
                        parts
                            .extensions
                            .insert(aioduct::Protocol::from_static("websocket"));
                    }
                    Case::Remove => {
                        parts.extensions.remove::<aioduct::Protocol>();
                    }
                    Case::Change => {
                        parts
                            .extensions
                            .insert(aioduct::Protocol::from_static("connect-udp"));
                    }
                    Case::Invalid => {}
                })
                .send()
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    aioduct::Error::Unsupported(_) | aioduct::Error::InvalidHeader(_)
                ),
                "{error}"
            );
        });

        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "invalid Local protocol metadata reached upstream I/O"
        );
    }
}

fn start_h2_connect_echo_server(
    expected_protocol: Option<&'static str>,
) -> (SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_connections = connections.clone();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    server_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::spawn(async move {
                        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                        let _ = hyper::server::conn::http2::Builder::new(
                            aioduct_test_server::TokioExec,
                        )
                        .enable_connect_protocol()
                        .serve_connection(
                            io,
                            service_fn(move |request| {
                                h2_connect_echo_response(request, expected_protocol)
                            }),
                        )
                        .await;
                    });
                }
            });
    });
    (rx.recv().unwrap(), connections)
}

#[cfg(feature = "rustls")]
fn start_tls_h2_connect_echo_server(
    expected_protocol: Option<&'static str>,
) -> (
    SocketAddr,
    rustls::pki_types::CertificateDer<'static>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    aioduct_test_server::tls::install_crypto_provider();
    let certificate = aioduct_test_server::tls::generate_self_signed(&["localhost"]);
    let certificate_der = certificate.cert_der.clone();
    let mut server_config =
        rustls::ServerConfig::builder_with_provider(aioduct_test_server::tls::crypto_provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.cert_der], certificate.key_der)
            .unwrap();
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));
    let negotiated_h2 = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_negotiated_h2 = negotiated_h2.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let acceptor = acceptor.clone();
                    let server_negotiated_h2 = server_negotiated_h2.clone();
                    tokio::spawn(async move {
                        let stream = acceptor.accept(stream).await.unwrap();
                        assert_eq!(
                            stream.get_ref().1.alpn_protocol(),
                            Some(b"h2".as_slice()),
                            "Local TLS CONNECT must negotiate HTTP/2 with ALPN"
                        );
                        server_negotiated_h2.store(true, std::sync::atomic::Ordering::Release);
                        let io = aioduct_test_server::TokioIo::new(stream);
                        let _ = hyper::server::conn::http2::Builder::new(
                            aioduct_test_server::TokioExec,
                        )
                        .enable_connect_protocol()
                        .serve_connection(
                            io,
                            service_fn(move |request| {
                                h2_connect_echo_response(request, expected_protocol)
                            }),
                        )
                        .await;
                    });
                }
            });
    });

    (rx.recv().unwrap(), certificate_der, negotiated_h2)
}

async fn assert_local_upgrade_echo(upgraded: &mut aioduct::UpgradedLocal, payload: &'static [u8]) {
    compio_runtime::time::timeout(Duration::from_secs(2), async {
        let mut remaining = payload;
        while !remaining.is_empty() {
            let written = std::future::poll_fn(|context| {
                hyper::rt::Write::poll_write(std::pin::Pin::new(&mut *upgraded), context, remaining)
            })
            .await
            .unwrap();
            assert_ne!(written, 0, "upgrade write returned zero");
            remaining = &remaining[written..];
        }
        std::future::poll_fn(|context| {
            hyper::rt::Write::poll_flush(std::pin::Pin::new(&mut *upgraded), context)
        })
        .await
        .unwrap();

        let mut echoed = vec![0_u8; payload.len()];
        let mut filled = 0;
        while filled < echoed.len() {
            let read = std::future::poll_fn(|context| {
                let mut buffer = hyper::rt::ReadBuf::new(&mut echoed[filled..]);
                match hyper::rt::Read::poll_read(
                    std::pin::Pin::new(&mut *upgraded),
                    context,
                    buffer.unfilled(),
                ) {
                    std::task::Poll::Ready(Ok(())) => {
                        std::task::Poll::Ready(Ok(buffer.filled().len()))
                    }
                    std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error)),
                    std::task::Poll::Pending => std::task::Poll::Pending,
                }
            })
            .await
            .unwrap();
            assert_ne!(read, 0, "upgrade closed before echo completed");
            filled += read;
        }
        assert_eq!(echoed, payload);
    })
    .await
    .expect("Local upgrade echo timed out");
}

#[cfg(feature = "rustls")]
fn assert_local_tls_h2_connect(expected_protocol: Option<&'static str>, payload: &'static [u8]) {
    let (upstream_addr, certificate, negotiated_h2) =
        start_tls_h2_connect_echo_server(expected_protocol);
    let connector = aioduct::tls::RustlsConnector::new(
        aioduct_test_server::tls::make_client_config(&certificate),
    );

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tls(connector)
            .build_local()
            .unwrap();
        let target = if expected_protocol.is_some() {
            "https://downstream.test/tunnel"
        } else {
            "target.example:443"
        };
        let ingress_version = if expected_protocol.is_some() {
            http::Version::HTTP_2
        } else {
            http::Version::HTTP_11
        };
        let mut request = Request::builder()
            .method(http::Method::CONNECT)
            .uri(target)
            .version(ingress_version)
            .body(Full::new(Bytes::new()))
            .unwrap();
        if expected_protocol.is_none() {
            request.headers_mut().insert(
                http::header::HOST,
                http::HeaderValue::from_static("target.example:443"),
            );
        }
        if let Some(protocol) = expected_protocol {
            request
                .extensions_mut()
                .insert(aioduct::Protocol::from_static(protocol));
        }

        let response = compio_runtime::time::timeout(
            Duration::from_secs(2),
            client
                .forward_local(super::valid_forward_request(request))
                .upstream(
                    format!("https://localhost:{}", upstream_addr.port())
                        .parse::<http::Uri>()
                        .unwrap(),
                )
                .on_request(|parts| parts.version = http::Version::HTTP_2)
                .send(),
        )
        .await
        .expect("Local TLS H2 CONNECT response timed out")
        .unwrap();
        assert_eq!(
            response.version(),
            ingress_version,
            "forwarding preserves the downstream-facing response version"
        );
        assert_eq!(response.status(), http::StatusCode::OK);

        let mut upgraded =
            compio_runtime::time::timeout(Duration::from_secs(2), response.upgrade())
                .await
                .expect("Local TLS H2 CONNECT upgrade timed out")
                .unwrap();
        assert_local_upgrade_echo(&mut upgraded, payload).await;
    });

    assert!(
        negotiated_h2.load(std::sync::atomic::Ordering::Acquire),
        "TLS server did not observe h2 ALPN"
    );
}

#[test]
fn local_h1_upgrade_survives_response_hook_extension_clear() {
    let (upstream_addr, connections) = start_reusable_upgrade_server();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let upstream = format!("http://{upstream_addr}")
            .parse::<http::Uri>()
            .unwrap();
        let warm = Request::builder()
            .method(http::Method::GET)
            .uri("/warm")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let warm = client
            .forward_local(warm)
            .upstream(upstream.clone())
            .on_request(|parts| parts.version = http::Version::HTTP_11)
            .send()
            .await
            .unwrap();
        assert_eq!(warm.text().await.unwrap(), "warm");
        for _ in 0..100 {
            if client.pool_stats().idle_pool_entries == 1 {
                break;
            }
            <CompioRuntime as aioduct::runtime::RuntimeCompletion>::sleep(Duration::from_millis(1))
                .await;
        }
        assert_eq!(client.pool_stats().idle_pool_entries, 1);

        let upgrade = Request::builder()
            .method(http::Method::GET)
            .uri("/upgrade")
            .header(http::header::CONNECTION, "upgrade")
            .header(http::header::UPGRADE, "test")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = client
            .forward_local(super::valid_forward_request(upgrade))
            .upstream(upstream)
            .on_response(|response| response.extensions_mut().clear())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);

        let mut upgraded = response.upgrade().await.unwrap();
        let written = std::future::poll_fn(|context| {
            hyper::rt::Write::poll_write(
                std::pin::Pin::new(&mut upgraded),
                context,
                b"reused upgrade",
            )
        })
        .await
        .unwrap();
        assert_eq!(written, b"reused upgrade".len());
        std::future::poll_fn(|context| {
            hyper::rt::Write::poll_flush(std::pin::Pin::new(&mut upgraded), context)
        })
        .await
        .unwrap();

        let mut echoed = [0; 32];
        let read = std::future::poll_fn(|context| {
            let mut buffer = hyper::rt::ReadBuf::new(&mut echoed);
            match hyper::rt::Read::poll_read(
                std::pin::Pin::new(&mut upgraded),
                context,
                buffer.unfilled(),
            ) {
                std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(buffer.filled().len())),
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error)),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        })
        .await
        .unwrap();
        assert_eq!(&echoed[..read], b"reused upgrade");
    });

    assert_eq!(
        connections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "ordinary request and upgrade should share one HTTP/1.1 connection"
    );
}

#[test]
fn local_forward_response_hook_cannot_change_selected_upgrade_protocol() {
    let (upstream_addr, _connections) = start_reusable_upgrade_server();

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let request = Request::builder()
            .method(http::Method::GET)
            .uri("/upgrade")
            .header(http::header::CONNECTION, "upgrade")
            .header(http::header::UPGRADE, "test, alternate")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = client
            .forward_local(super::valid_forward_request(request))
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_response(|response| {
                response.headers_mut().insert(
                    http::header::UPGRADE,
                    http::HeaderValue::from_static("alternate"),
                );
            })
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
        assert!(error.to_string().contains("upstream-selected"), "{error}");
    });
}

#[test]
fn local_h2_connect_survives_response_hook_extension_clear_and_retains_capacity() {
    let (upstream_addr, connections) = start_h2_connect_echo_server(Some("websocket"));

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .pool_max_active_streams_per_connection(1)
            .build_local()
            .unwrap();
        let mut request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("http://downstream.test/tunnel")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();
        request
            .extensions_mut()
            .insert(aioduct::Protocol::from_static("websocket"));

        let response = client
            .forward_local(request)
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .on_response(|response| response.extensions_mut().clear())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);

        let mut upgraded = response.upgrade().await.unwrap();
        assert_local_upgrade_echo(&mut upgraded, b"local h2 tunnel").await;

        let ordinary = client
            .get_local(&format!("http://{upstream_addr}/ordinary"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
        assert_eq!(ordinary.text().await.unwrap(), "ordinary");
        assert_eq!(
            connections.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "an extended CONNECT tunnel must retain its active-stream capacity"
        );
    });
}

#[test]
fn local_h2_ordinary_connect_reuses_transport_while_tunnel_is_open() {
    let (upstream_addr, connections) = start_h2_connect_echo_server(None);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .pool_max_active_streams_per_connection(2)
            .build_local()
            .unwrap();
        let request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .forward_local(request)
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);

        let mut upgraded = response.upgrade().await.unwrap();
        assert_local_upgrade_echo(&mut upgraded, b"local ordinary h2 tunnel").await;

        let ordinary = client
            .get_local(&format!("http://{upstream_addr}/ordinary"))
            .unwrap()
            .h2c_prior_knowledge()
            .send()
            .await
            .unwrap();
        assert_eq!(ordinary.text().await.unwrap(), "ordinary");
        assert_eq!(
            connections.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a successful Local H2 CONNECT must leave the fresh transport poolable"
        );
    });
}

#[test]
fn no_connection_reuse_keeps_local_h2_ordinary_connect_tunnel_alive() {
    let (upstream_addr, connections) = start_h2_connect_echo_server(None);

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .no_connection_reuse()
            .build_local()
            .unwrap();
        let request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = client
            .forward_local(request)
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);

        let mut upgraded =
            compio_runtime::time::timeout(Duration::from_secs(2), response.upgrade())
                .await
                .expect("Local H2 CONNECT upgrade timed out")
                .unwrap();
        assert_local_upgrade_echo(&mut upgraded, b"unpooled local h2 tunnel").await;
        assert_eq!(client.pool_stats().idle_pool_entries, 0);
    });

    assert_eq!(
        connections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the unpooled CONNECT should use exactly one live H2 connection"
    );
}

#[cfg(feature = "rustls")]
#[test]
fn local_tls_h2_ordinary_connect_negotiates_alpn_and_round_trips() {
    assert_local_tls_h2_connect(None, b"local TLS ordinary h2 tunnel");
}

#[cfg(feature = "rustls")]
#[test]
fn local_tls_h2_extended_connect_negotiates_alpn_and_round_trips() {
    assert_local_tls_h2_connect(Some("websocket"), b"local TLS extended h2 tunnel");
}
