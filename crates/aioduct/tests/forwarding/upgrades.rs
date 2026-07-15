#[path = "upgrades/incoming/mod.rs"]
mod incoming;

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(all(feature = "rustls", feature = "http3"))]
use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::runtime::{ConnectorSend, TokioRuntime};
use aioduct_test_server::TokioExec;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

#[derive(Clone)]
struct ForbiddenIoConnector {
    attempts: tokio::sync::mpsc::UnboundedSender<SocketAddr>,
}

impl ConnectorSend for ForbiddenIoConnector {
    type Stream = <TcpConnector as ConnectorSend>::Stream;

    fn connect(&self, addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let _ = self.attempts.send(addr);
        async { Err(io::Error::other("pre-I/O validation reached the connector")) }
    }

    fn connect_bound(
        &self,
        addr: SocketAddr,
        _local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        let _ = self.attempts.send(addr);
        async { Err(io::Error::other("pre-I/O validation reached the connector")) }
    }
}

async fn hyper_write_all<T: hyper::rt::Write + Unpin>(
    io: &mut T,
    mut bytes: &[u8],
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written = std::future::poll_fn(|cx| {
            hyper::rt::Write::poll_write(std::pin::Pin::new(&mut *io), cx, bytes)
        })
        .await?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
    }
    std::future::poll_fn(|cx| hyper::rt::Write::poll_flush(std::pin::Pin::new(&mut *io), cx)).await
}

async fn hyper_read_exact<T: hyper::rt::Read + Unpin>(
    io: &mut T,
    bytes: &mut [u8],
) -> std::io::Result<()> {
    let mut read = 0;
    while read < bytes.len() {
        let mut buffer = hyper::rt::ReadBuf::new(&mut bytes[read..]);
        std::future::poll_fn(|cx| {
            hyper::rt::Read::poll_read(std::pin::Pin::new(&mut *io), cx, buffer.unfilled())
        })
        .await?;
        let received = buffer.filled().len();
        if received == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        read += received;
    }
    Ok(())
}

async fn raw_switching_protocols_upstream(response_headers: &'static str) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(
                format!("HTTP/1.1 101 Switching Protocols\r\n{response_headers}\r\n").as_bytes(),
            )
            .await
            .unwrap();
    });
    addr
}

async fn raw_echo_switching_protocols_upstream(
    response_headers: &'static str,
) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(
                format!("HTTP/1.1 101 Switching Protocols\r\n{response_headers}\r\n").as_bytes(),
            )
            .await
            .unwrap();
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            stream.write_all(&buffer[..read]).await.unwrap();
        }
    });
    addr
}

#[tokio::test]
async fn forward_h1_upgrade_restores_host_after_connection_option_sanitization() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
        }
        request_tx.send(request).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });

    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/chat")
        .version(http::Version::HTTP_11)
        .header(http::header::HOST, "broker.test")
        .header(http::header::CONNECTION, "upgrade, host")
        .header(http::header::UPGRADE, "websocket")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);

    let request = String::from_utf8(request_rx.await.unwrap()).unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(
        lower.contains(&format!("\r\nhost: {addr}\r\n")),
        "{request}"
    );
    assert!(lower.contains("\r\nconnection: upgrade\r\n"), "{request}");
    assert!(!lower.contains("connection: upgrade, host"), "{request}");
}

#[tokio::test]
async fn forward_forced_h1_upgrade_rejects_missing_required_headers_before_io() {
    let (attempt_tx, mut attempt_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = HttpEngineSend::<TokioRuntime, ForbiddenIoConnector>::builder_with_connector(
        ForbiddenIoConnector {
            attempts: attempt_tx,
        },
    )
    .build()
    .unwrap();
    let upstream = "http://127.0.0.1:9".parse::<http::Uri>().unwrap();

    let missing_upgrade = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(http::header::CONNECTION, "upgrade")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let error = client
        .forward(crate::valid_forward_request(missing_upgrade))
        .upstream(upstream.clone())
        .upgrade()
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, aioduct::Error::InvalidHeader(_)));

    let missing_connection = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(http::header::UPGRADE, "websocket")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let error = client
        .forward(crate::valid_forward_request(missing_connection))
        .upstream(upstream)
        .upgrade()
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, aioduct::Error::InvalidHeader(_)));

    assert!(
        matches!(
            attempt_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "invalid upgrade metadata reached the upstream connector"
    );
}

#[tokio::test]
async fn forward_h1_upgrade_rejects_hook_created_or_disjoint_offers_before_io() {
    let (attempt_tx, mut attempt_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = HttpEngineSend::<TokioRuntime, ForbiddenIoConnector>::builder_with_connector(
        ForbiddenIoConnector {
            attempts: attempt_tx,
        },
    )
    .build()
    .unwrap();
    let upstream = "http://127.0.0.1:9".parse::<http::Uri>().unwrap();

    for (downstream_offer, hook_offer) in
        [(None, "websocket"), (Some("websocket"), "upstream-chat")]
    {
        let mut request = Request::builder().method(http::Method::GET).uri("/upgrade");
        if let Some(downstream_offer) = downstream_offer {
            request = request
                .header(http::header::CONNECTION, "upgrade")
                .header(http::header::UPGRADE, downstream_offer);
        }
        let request = request.body(Full::new(Bytes::new())).unwrap();

        let error = client
            .forward(crate::valid_forward_request(request))
            .upstream(upstream.clone())
            .on_request(move |parts| {
                parts.headers.insert(
                    http::header::CONNECTION,
                    http::HeaderValue::from_static("upgrade"),
                );
                parts.headers.insert(
                    http::header::UPGRADE,
                    http::HeaderValue::from_static(hook_offer),
                );
            })
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    }

    assert!(
        matches!(
            attempt_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "hook-created or disjoint upgrade metadata reached the upstream connector"
    );
}

#[tokio::test]
async fn forward_extended_connect_rejects_invalid_or_hook_mutated_protocol_before_io() {
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

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
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
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "invalid protocol metadata reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn forward_h1_upgrade_constrains_an_expanded_hook_offer() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|request: Request<hyper::body::Incoming>| async move {
                    let connection = request
                        .headers()
                        .get(http::header::CONNECTION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default();
                    let upgrade = request
                        .headers()
                        .get_all(http::header::UPGRADE)
                        .iter()
                        .filter_map(|value| value.to_str().ok())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "{connection}|{upgrade}"
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(http::header::CONNECTION, "upgrade")
        .header(http::header::UPGRADE, "websocket")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| {
            parts.headers.insert(
                http::header::UPGRADE,
                http::HeaderValue::from_static("websocket, upstream-chat"),
            );
        })
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "upgrade|websocket");
}

#[tokio::test]
async fn forward_h1_upgrade_survives_response_hook_extension_clear() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|mut req: Request<hyper::body::Incoming>| async move {
                    if req.headers().get("upgrade").map(|v| v.as_bytes()) == Some(b"websocket") {
                        tokio::spawn(async move {
                            if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
                                let mut upgraded = aioduct::UpgradedSend::from(upgraded);
                                let mut buf = vec![0u8; 64];
                                let n = AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
                                AsyncWriteExt::write_all(&mut upgraded, &buf[..n])
                                    .await
                                    .unwrap();
                            }
                        });
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(101)
                                .header("connection", "Upgrade")
                                .header("upgrade", "websocket")
                                .body(Full::new(Bytes::new()))
                                .unwrap(),
                        )
                    } else {
                        Ok(Response::new(Full::new(Bytes::from("not an upgrade"))))
                    }
                }),
            )
            .with_upgrades()
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/ws")
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_response(|response| response.extensions_mut().clear())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);
    assert!(resp.headers().get("upgrade").is_some());
    assert!(resp.headers().get("connection").is_some());

    let mut upgraded = resp.upgrade().await.unwrap();
    AsyncWriteExt::write_all(&mut upgraded, b"hello ws")
        .await
        .unwrap();
    let mut buf = vec![0u8; 64];
    let n = AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello ws");
}

#[tokio::test]
async fn forward_h1_upgrade_accepts_and_preserves_multiple_protocol_layers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = raw_echo_switching_protocols_upstream(
        "Connection: Upgrade\r\nUpgrade: transport/1, application/2\r\n",
    )
    .await;
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(http::header::CONNECTION, "upgrade")
        .header(http::header::UPGRADE, "transport/1, application/2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        response.headers().get(http::header::UPGRADE).unwrap(),
        "transport/1, application/2"
    );
    let mut upgraded = tokio::time::timeout(std::time::Duration::from_secs(2), response.upgrade())
        .await
        .expect("multi-layer upgrade handoff timed out")
        .unwrap();
    upgraded.write_all(b"layered").await.unwrap();
    upgraded.flush().await.unwrap();
    let mut echoed = [0_u8; 7];
    upgraded.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"layered");
}

#[tokio::test]
async fn forward_h1_upgrade_rejects_an_unoffered_protocol_layer() {
    let addr = raw_switching_protocols_upstream(
        "Connection: Upgrade\r\nUpgrade: transport/1, unoffered/2\r\n",
    )
    .await;
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(http::header::CONNECTION, "upgrade")
        .header(http::header::UPGRADE, "transport/1, application/2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    assert!(error.to_string().contains("unoffered/2"), "{error}");
}

#[tokio::test]
async fn forward_rejects_unsolicited_incomplete_and_mismatched_h1_switches() {
    for (request_upgrade, response_headers) in [
        (None, "Connection: Upgrade\r\nUpgrade: websocket\r\n"),
        (Some("websocket"), "Connection: Upgrade\r\nUpgrade: h2c\r\n"),
        (Some("websocket"), "Upgrade: websocket\r\n"),
        (Some("websocket"), "Connection: Upgrade\r\n"),
        (
            Some("websocket"),
            "Connection: Upgrade\r\nUpgrade: websocket, h2c\r\n",
        ),
    ] {
        let addr = raw_switching_protocols_upstream(response_headers).await;
        let mut request = Request::builder().method(http::Method::GET).uri("/upgrade");
        if let Some(upgrade) = request_upgrade {
            request = request
                .header(http::header::CONNECTION, "upgrade")
                .header(http::header::UPGRADE, upgrade);
        }
        let request = request.body(Full::new(Bytes::new())).unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                aioduct::Error::InvalidHeader(_) | aioduct::Error::Hyper(_)
            ),
            "unexpected error for {response_headers:?}: {error}"
        );
    }
}

#[tokio::test]
async fn forward_validates_switch_against_the_constrained_upstream_offer() {
    let addr =
        raw_switching_protocols_upstream("Connection: Upgrade\r\nUpgrade: upstream-chat\r\n").await;
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(http::header::CONNECTION, "upgrade")
        .header(http::header::UPGRADE, "websocket, upstream-chat")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_request(|parts| {
            parts.headers.insert(
                http::header::UPGRADE,
                http::HeaderValue::from_static("websocket"),
            );
        })
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    assert!(error.to_string().contains("unoffered"), "{error}");
}

#[tokio::test]
async fn forward_response_hook_cannot_change_selected_upgrade_protocol() {
    let addr =
        raw_switching_protocols_upstream("Connection: Upgrade\r\nUpgrade: websocket\r\n").await;
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/upgrade")
        .header(http::header::CONNECTION, "upgrade")
        .header(http::header::UPGRADE, "websocket, chat-v2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_response(|response| {
            response.headers_mut().insert(
                http::header::UPGRADE,
                http::HeaderValue::from_static("chat-v2"),
            );
        })
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    assert!(error.to_string().contains("upstream-selected"), "{error}");
}

#[tokio::test]
async fn forward_h1_upgrade_preserves_headers() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let has_connection = req.headers().contains_key("connection");
                    let has_upgrade = req.headers().contains_key("upgrade");
                    let upgrade_val = req
                        .headers()
                        .get("upgrade")
                        .map(|v| v.to_str().unwrap().to_owned())
                        .unwrap_or_default();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "conn={},upgrade={},val={}",
                        has_connection, has_upgrade, upgrade_val
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/ws")
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert_eq!(body, "conn=true,upgrade=true,val=websocket");
}

#[tokio::test]
async fn forward_h2c_upgrade_preserves_http2_settings_handshake() {
    let (upstream_addr, _) = aioduct_test_server::h1::h1_server_with(|req| async move {
        let value = |name| {
            req.headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("missing")
                .to_owned()
        };
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{}|{}|{}",
            value("connection"),
            value("upgrade"),
            value("http2-settings")
        )))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/h2c")
        .version(http::Version::HTTP_11)
        .header("connection", "keep-alive, Upgrade, HTTP2-Settings")
        .header("upgrade", "h2c")
        .header("http2-settings", "AAEAAAAA")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        "upgrade, http2-settings|h2c|AAEAAAAA"
    );
}

#[tokio::test]
async fn forward_upgrade_field_without_connection_upgrade_token_strips_upgrade_fields() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let has_connection = req.headers().contains_key("connection");
                    let has_upgrade = req.headers().contains_key("upgrade");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "conn={},upgrade={}",
                        has_connection, has_upgrade
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/h2c-probe")
        .header("connection", "keep-alive")
        .header("upgrade", "h2c")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "conn=false,upgrade=false");
}

#[tokio::test]
async fn forward_non_upgrade_still_strips_connection() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let has_connection = req.headers().contains_key("connection");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "conn={}",
                        has_connection
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("connection", "keep-alive")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "conn=false");
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_h3_extended_connect_protocol_is_rejected_before_upstream_io() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    let mut request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("https://downstream.test/ws")
        .version(http::Version::HTTP_3)
        .body(Full::new(Bytes::new()))
        .unwrap();
    request
        .extensions_mut()
        .insert(h3::ext::Protocol::CONNECT_UDP);

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(request)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("HTTP/3 extended CONNECT")),
        "{error:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), upstream.accept())
            .await
            .is_err(),
        "unsupported HTTP/3 extended CONNECT reached the upstream"
    );
}

#[tokio::test]
async fn forward_h2_extended_connect_survives_response_hook_extension_clear() {
    use hyper::server::conn::http2 as server_http2;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = connections.clone();

    tokio::spawn(async move {
        loop {
            let (stream, _) = upstream.accept().await.unwrap();
            server_connections.fetch_add(1, Ordering::SeqCst);
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http2::Builder::new(TokioExec)
                    .enable_connect_protocol()
                    .serve_connection(
                        io,
                        service_fn(move |mut req: Request<hyper::body::Incoming>| {
                            let expected_authority = upstream_addr.to_string();
                            async move {
                                if req.method() == http::Method::CONNECT {
                                    assert_eq!(
                                        req.extensions()
                                            .get::<hyper::ext::Protocol>()
                                            .map(hyper::ext::Protocol::as_str),
                                        Some("websocket")
                                    );
                                    assert_eq!(req.uri().scheme_str(), Some("http"));
                                    assert_eq!(
                                        req.uri().authority().map(http::uri::Authority::as_str),
                                        Some(expected_authority.as_str())
                                    );
                                    assert_eq!(req.uri().path(), "/ws/chat");
                                    tokio::spawn(async move {
                                        if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
                                            let mut io = aioduct::UpgradedSend::from(upgraded);
                                            let mut buf = vec![0u8; 1024];
                                            loop {
                                                let n = match AsyncReadExt::read(&mut io, &mut buf)
                                                    .await
                                                {
                                                    Ok(0) | Err(_) => break,
                                                    Ok(n) => n,
                                                };
                                                if AsyncWriteExt::write_all(&mut io, &buf[..n])
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                        }
                                    });
                                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                                } else {
                                    Ok(Response::new(Full::new(Bytes::from("expected CONNECT"))))
                                }
                            }
                        }),
                    )
                    .await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_max_active_streams_per_connection(1)
        .build()
        .unwrap();

    let mut incoming_req = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri(format!("http://127.0.0.1:{}/ws/chat", upstream_addr.port()))
        .body(Full::new(Bytes::new()))
        .unwrap();
    incoming_req
        .extensions_mut()
        .insert(aioduct::Protocol::from_static("websocket"));

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .on_response(|response| response.extensions_mut().clear())
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);

    let mut upgraded = resp.upgrade().await.unwrap();
    AsyncWriteExt::write_all(&mut upgraded, b"h2 tunnel test")
        .await
        .unwrap();
    let mut buf = vec![0u8; 64];
    let n = AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"h2 tunnel test");

    let ordinary = client
        .get(&format!("http://{upstream_addr}/ordinary"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(ordinary.text().await.unwrap(), "expected CONNECT");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "an upgraded H2 CONNECT stream must retain its capacity permit"
    );
}

#[tokio::test]
async fn forward_h2_ordinary_connect_reuses_transport_while_tunnel_is_open() {
    use hyper::server::conn::http2 as server_http2;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = connections.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = upstream.accept().await.unwrap();
            server_connections.fetch_add(1, Ordering::SeqCst);
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http2::Builder::new(TokioExec)
                    .enable_connect_protocol()
                    .serve_connection(
                        io,
                        service_fn(|mut request: Request<hyper::body::Incoming>| async move {
                            if request.method() != http::Method::CONNECT {
                                return Ok::<_, Infallible>(Response::new(Full::new(
                                    Bytes::from_static(b"ordinary"),
                                )));
                            }
                            assert!(request.extensions().get::<hyper::ext::Protocol>().is_none());
                            assert_eq!(
                                request.uri().authority().map(http::uri::Authority::as_str),
                                Some("target.example:443")
                            );
                            tokio::spawn(async move {
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
                        }),
                    )
                    .await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_max_active_streams_per_connection(2)
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = client
        .forward(request)
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

    let mut upgraded = tokio::time::timeout(std::time::Duration::from_secs(2), response.upgrade())
        .await
        .expect("ordinary H2 CONNECT upgrade timed out")
        .unwrap();
    upgraded.write_all(b"ordinary h2 tunnel").await.unwrap();
    upgraded.flush().await.unwrap();
    let mut echoed = [0_u8; 18];
    upgraded.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ordinary h2 tunnel");

    let ordinary = client
        .get(&format!("http://{upstream_addr}/ordinary"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(ordinary.text().await.unwrap(), "ordinary");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "a successful H2 CONNECT must leave the fresh transport poolable"
    );
}

#[tokio::test]
async fn raw_h2_connect_response_retires_transport_before_permit_release() {
    let (upstream_addr, connections) = aioduct_test_server::h2::h2_server_with(
        |mut request: Request<hyper::body::Incoming>| async move {
            if request.method() == http::Method::CONNECT {
                tokio::spawn(async move {
                    let _upgraded = hyper::upgrade::on(&mut request).await;
                });
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
            } else {
                Ok(Response::new(Full::new(Bytes::from_static(b"ordinary"))))
            }
        },
    )
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_max_active_streams_per_connection(1)
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = client
        .forward(request)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    let mut raw = response.into_http_response();
    let upgraded = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        hyper::upgrade::on(&mut raw),
    )
    .await
    .expect("raw H2 CONNECT upgrade timed out")
    .unwrap();
    drop(raw);
    let _upgraded = aioduct::UpgradedSend::from(upgraded);

    let ordinary = client
        .get(&format!("http://{upstream_addr}/ordinary"))
        .unwrap()
        .h2c_prior_knowledge()
        .send()
        .await
        .unwrap();
    assert_eq!(ordinary.text().await.unwrap(), "ordinary");
    assert_eq!(
        connections.connections(),
        2,
        "raw upgrade extraction must retire the H2 transport before releasing capacity"
    );
}

#[tokio::test]
async fn no_connection_reuse_keeps_h2_connect_tunnel_alive() {
    use hyper::server::conn::http2 as server_http2;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let mut builder = server_http2::Builder::new(TokioExec);
        builder.enable_connect_protocol();
        let _ = builder
            .serve_connection(
                io,
                service_fn(|mut request: Request<hyper::body::Incoming>| async move {
                    assert_eq!(request.method(), http::Method::CONNECT);
                    tokio::spawn(async move {
                        let mut upgraded = aioduct::UpgradedSend::from(
                            hyper::upgrade::on(&mut request).await.unwrap(),
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        let mut buffer = [0_u8; 32];
                        let read = upgraded.read(&mut buffer).await.unwrap();
                        upgraded.write_all(&buffer[..read]).await.unwrap();
                    });
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                }),
            )
            .await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .no_connection_reuse()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = client
        .forward(request)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();
    let mut upgraded = response.upgrade().await.unwrap().into_inner();
    hyper_write_all(&mut upgraded, b"delayed tunnel")
        .await
        .unwrap();
    let mut echoed = [0_u8; 14];
    hyper_read_exact(&mut upgraded, &mut echoed).await.unwrap();

    assert_eq!(&echoed, b"delayed tunnel");
    assert_eq!(client.pool_stats().idle_pool_entries, 0);
}

#[tokio::test]
async fn forward_h2_connect_rejects_non_200_success_without_a_false_tunnel() {
    use hyper::server::conn::http2 as server_http2;

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = upstream.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http2::Builder::new(TokioExec)
                    .enable_connect_protocol()
                    .serve_connection(
                        io,
                        service_fn(|request: Request<hyper::body::Incoming>| async move {
                            assert_eq!(request.method(), http::Method::CONNECT);
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(http::StatusCode::CREATED)
                                    .body(Full::new(Bytes::new()))
                                    .unwrap(),
                            )
                        }),
                    )
                    .await;
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let ordinary = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();
    let error = client
        .forward(ordinary)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(&error, aioduct::Error::Unsupported(message) if message.contains("requires status 200")),
        "{error}"
    );

    let mut extended = Request::builder()
        .method(http::Method::CONNECT)
        .uri("http://downstream.test/ws")
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();
    extended
        .extensions_mut()
        .insert(aioduct::Protocol::from_static("websocket"));
    let error = client
        .forward(extended)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(&error, aioduct::Error::Unsupported(message) if message.contains("requires status 200")),
        "{error}"
    );
}
