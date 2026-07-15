use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};

const CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

async fn read_h1_request_head(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;

    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = tokio::time::timeout(CLEANUP_TIMEOUT, stream.read(&mut buffer))
            .await
            .expect("HTTP/1.1 request head timed out")
            .unwrap();
        assert_ne!(read, 0, "client closed before sending request headers");
        request.extend_from_slice(&buffer[..read]);
    }
    request
}

async fn start_h1_connect_cleanup_server() -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut tunnel, _) = tokio::time::timeout(CLEANUP_TIMEOUT, listener.accept())
            .await
            .expect("CONNECT connection was not accepted")
            .unwrap();
        let request = read_h1_request_head(&mut tunnel).await;
        assert!(
            request.starts_with(b"CONNECT target.example:443 HTTP/1.1\r\n"),
            "unexpected CONNECT request: {}",
            String::from_utf8_lossy(&request)
        );
        tunnel
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        tunnel.flush().await.unwrap();

        let mut byte = [0_u8; 1];
        match tokio::time::timeout(CLEANUP_TIMEOUT, tunnel.read(&mut byte)).await {
            Ok(Ok(0) | Err(_)) => {}
            Ok(Ok(read)) => panic!("rejected CONNECT tunnel remained writable ({read} byte)"),
            Err(_) => panic!("response-hook failure did not close the CONNECT tunnel"),
        }
        closed_tx.send(()).unwrap();

        let (mut follow_up, _) = tokio::time::timeout(CLEANUP_TIMEOUT, listener.accept())
            .await
            .expect("follow-up connection was not accepted")
            .unwrap();
        let request = read_h1_request_head(&mut follow_up).await;
        assert!(
            request.starts_with(b"GET /after HTTP/1.1\r\n"),
            "unexpected follow-up request: {}",
            String::from_utf8_lossy(&request)
        );
        follow_up
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nafter")
            .await
            .unwrap();
    });

    (addr, closed_rx, server)
}

async fn start_h2_connect_cleanup_server() -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (reset_tx, reset_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = tokio::time::timeout(CLEANUP_TIMEOUT, listener.accept())
            .await
            .expect("H2 CONNECT connection was not accepted")
            .unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();
        let (request, mut respond) = tokio::time::timeout(CLEANUP_TIMEOUT, connection.accept())
            .await
            .expect("H2 CONNECT request timed out")
            .unwrap()
            .unwrap();
        assert_eq!(request.method(), http::Method::CONNECT);
        assert_eq!(request.uri().authority().unwrap(), "target.example:443");
        let mut tunnel = respond
            .send_response(Response::builder().status(200).body(()).unwrap(), false)
            .unwrap();

        let reset = std::future::poll_fn(|context| tunnel.poll_reset(context));
        tokio::pin!(reset);
        let reason = tokio::select! {
            biased;
            reason = &mut reset => reason,
            accepted = connection.accept() => match accepted {
                Some(Ok((request, _))) => {
                    panic!("request arrived before rejected CONNECT reset: {request:?}")
                }
                Some(Err(error)) => panic!("H2 connection failed before reset: {error}"),
                None => panic!("response-hook failure closed the entire H2 connection"),
            },
            _ = tokio::time::sleep(CLEANUP_TIMEOUT) => {
                panic!("response-hook failure did not reset the H2 CONNECT stream")
            }
        };
        assert_eq!(reason.unwrap(), h2::Reason::CANCEL);
        drop(tunnel);
        reset_tx.send(()).unwrap();

        enum FollowUp {
            Existing(Box<(Request<h2::RecvStream>, h2::server::SendResponse<Bytes>)>),
            Fresh(tokio::net::TcpStream),
        }
        let follow_up = tokio::select! {
            accepted = connection.accept() => match accepted {
                Some(Ok(request)) => FollowUp::Existing(Box::new(request)),
                Some(Err(_)) | None => {
                    let (stream, _) = listener.accept().await.unwrap();
                    FollowUp::Fresh(stream)
                }
            },
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                FollowUp::Fresh(stream)
            },
            _ = tokio::time::sleep(CLEANUP_TIMEOUT) => {
                panic!("H2 follow-up request timed out")
            }
        };
        match follow_up {
            FollowUp::Existing(existing) => {
                let (request, mut respond) = *existing;
                assert_eq!(request.uri().path(), "/after");
                respond
                    .send_response(Response::builder().status(200).body(()).unwrap(), true)
                    .unwrap();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    connection.accept(),
                )
                .await;
            }
            FollowUp::Fresh(stream) => {
                let mut fresh = h2::server::handshake(stream).await.unwrap();
                let (request, mut respond) = tokio::time::timeout(CLEANUP_TIMEOUT, fresh.accept())
                    .await
                    .expect("fresh H2 follow-up request timed out")
                    .unwrap()
                    .unwrap();
                assert_eq!(request.uri().path(), "/after");
                respond
                    .send_response(Response::builder().status(200).body(()).unwrap(), true)
                    .unwrap();
                let _ = tokio::time::timeout(std::time::Duration::from_millis(100), fresh.accept())
                    .await;
            }
        }
    });

    (addr, reset_rx, server)
}

#[tokio::test]
async fn forward_send_on_response_cannot_promote_failed_connect_to_success() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|request| async move {
        let status = if request.method() == http::Method::CONNECT {
            http::StatusCode::PROXY_AUTHENTICATION_REQUIRED
        } else {
            assert_eq!(request.uri().path(), "/replacement");
            http::StatusCode::OK
        };
        Ok::<_, std::convert::Infallible>(
            Response::builder()
                .status(status)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let replacement = client
        .get(&format!("http://{addr}/replacement"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_11)
        .header(http::header::HOST, "target.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_response(move |response| *response = replacement)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(error, aioduct::Error::InvalidHeader(ref message) if message.contains("establishes a tunnel")),
        "{error}"
    );
    assert_eq!(counter.requests(), 2);
}

#[tokio::test]
async fn forward_send_on_response_cannot_demote_h1_connect_and_closes_tunnel() {
    let (addr, closed, server) = start_h1_connect_cleanup_server().await;
    let (replacement_addr, _) = aioduct_test_server::h1::h1_server_with(|_request| async move {
        Ok::<_, std::convert::Infallible>(
            Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let replacement = client
        .get(&format!("http://{replacement_addr}/replacement"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_11)
        .header(http::header::HOST, "target.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        client
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .on_response(move |response| *response = replacement)
            .send(),
    )
    .await
    .expect("CONNECT response hook timed out")
    .unwrap_err();
    assert!(
        matches!(error, aioduct::Error::InvalidHeader(ref message) if message.contains("establishes a tunnel")),
        "{error}"
    );
    tokio::time::timeout(CLEANUP_TIMEOUT, closed)
        .await
        .expect("rejected CONNECT tunnel did not close")
        .unwrap();

    let follow_up = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        client.get(&format!("http://{addr}/after")).unwrap().send(),
    )
    .await
    .expect("follow-up request timed out")
    .unwrap();
    assert_eq!(follow_up.text().await.unwrap(), "after");
    server.await.unwrap();
}

#[tokio::test]
async fn forward_send_on_response_cannot_demote_h2_connect_and_releases_stream_permit() {
    let (addr, reset, server) = start_h2_connect_cleanup_server().await;
    let (replacement_addr, _) = aioduct_test_server::h1::h1_server_with(|_request| async move {
        Ok::<_, std::convert::Infallible>(
            Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .pool_max_active_streams_per_connection(1)
        .build()
        .unwrap();
    let replacement = client
        .get(&format!("http://{replacement_addr}/replacement"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        client
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .on_response(move |response| *response = replacement)
            .send(),
    )
    .await
    .expect("H2 CONNECT response hook timed out")
    .unwrap_err();
    assert!(
        matches!(error, aioduct::Error::InvalidHeader(ref message) if message.contains("establishes a tunnel")),
        "{error}"
    );
    tokio::time::timeout(CLEANUP_TIMEOUT, reset)
        .await
        .expect("rejected H2 CONNECT stream was not reset")
        .unwrap();

    let follow_up = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        client
            .get(&format!("http://{addr}/after"))
            .unwrap()
            .h2c_prior_knowledge()
            .send(),
    )
    .await
    .expect("H2 follow-up request timed out")
    .unwrap();
    assert_eq!(follow_up.status(), http::StatusCode::OK);
    server.await.unwrap();
}

#[tokio::test]
async fn forward_h2_connect_rejects_success_statuses_without_upgrade_support() {
    for status in [
        http::StatusCode::CREATED,
        http::StatusCode::NO_CONTENT,
        http::StatusCode::from_u16(299).unwrap(),
    ] {
        let (addr, _) = aioduct_test_server::h2::h2_server_with(move |_request| async move {
            Ok::<_, std::convert::Infallible>(
                Response::builder()
                    .status(status)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        })
        .await;
        let request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            HttpEngineSend::<TokioRuntime, TcpConnector>::new()
                .forward(crate::valid_forward_request(request))
                .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
                .h2c()
                .send(),
        )
        .await
        .expect("HTTP/2 CONNECT response timed out")
        .unwrap_err();

        assert!(
            matches!(&error, aioduct::Error::Unsupported(message) if message.contains("requires status 200")),
            "unexpected error for {status}: {error}"
        );
    }
}

#[tokio::test]
async fn rejected_h2_connect_preserves_unrelated_stream_and_retires_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();

        let (warm, mut respond) = connection.accept().await.unwrap().unwrap();
        assert_eq!(warm.uri().path(), "/warm");
        respond
            .send_response(Response::builder().status(200).body(()).unwrap(), true)
            .unwrap();

        let first = connection.accept().await.unwrap().unwrap();
        let second = connection.accept().await.unwrap().unwrap();
        let ((connect, mut connect_response), (unrelated, mut unrelated_response)) =
            if first.0.method() == http::Method::CONNECT {
                (first, second)
            } else {
                (second, first)
            };
        assert_eq!(connect.method(), http::Method::CONNECT);
        assert_eq!(unrelated.uri().path(), "/unrelated");

        let mut tunnel = connect_response
            .send_response(Response::builder().status(201).body(()).unwrap(), false)
            .unwrap();
        let mut unrelated_body = unrelated_response
            .send_response(Response::builder().status(200).body(()).unwrap(), false)
            .unwrap();
        let mut reset = tokio::spawn(async move {
            std::future::poll_fn(|cx| tunnel.poll_reset(cx))
                .await
                .unwrap()
        });
        let connection_closed = tokio::select! {
            reason = &mut reset => {
                assert_eq!(reason.unwrap(), h2::Reason::CANCEL);
                false
            }
            accepted = connection.accept() => match accepted {
                Some(Ok((request, _))) => {
                    panic!("unexpected request before CONNECT reset: {request:?}")
                }
                Some(Err(error)) => panic!("first H2 connection failed: {error}"),
                None => true,
            }
        };

        if connection_closed {
            panic!("retiring the rejected CONNECT closed an unrelated response stream");
        } else {
            tokio::select! {
            accepted = connection.accept() => {
                match accepted {
                    Some(Ok((request, _))) => panic!(
                        "rejected CONNECT transport was reused for {}",
                        request.uri()
                    ),
                    Some(Err(error)) => panic!("first H2 connection failed: {error}"),
                    None => {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut fresh = h2::server::handshake(stream).await.unwrap();
                        let (request, mut respond) = fresh.accept().await.unwrap().unwrap();
                        assert_eq!(request.uri().path(), "/after");
                        respond
                            .send_response(Response::builder().status(200).body(()).unwrap(), true)
                            .unwrap();
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(50),
                            fresh.accept(),
                        )
                        .await;
                    }
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                let mut fresh = h2::server::handshake(stream).await.unwrap();
                let (request, mut respond) = fresh.accept().await.unwrap().unwrap();
                assert_eq!(request.uri().path(), "/after");
                respond
                    .send_response(Response::builder().status(200).body(()).unwrap(), true)
                    .unwrap();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    fresh.accept(),
                )
                .await;
            }
            }
            let _ = tokio::time::timeout(std::time::Duration::from_millis(50), connection.accept())
                .await;
        }
        unrelated_body
            .send_data(Bytes::from_static(b"unrelated body"), true)
            .expect("unrelated response stream must survive CONNECT retirement");
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(200), connection.accept()).await;
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let warm_request = Request::builder()
        .uri("/warm")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let warm = client
        .forward(crate::valid_forward_request(warm_request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .h2c()
        .send()
        .await
        .unwrap();
    assert_eq!(warm.status(), http::StatusCode::OK);
    drop(warm);
    for _ in 0..100 {
        if client.pool_stats().idle_pool_entries == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(client.pool_stats().idle_pool_entries, 1);

    let unrelated = async {
        let request = Request::builder()
            .uri("/unrelated")
            .body(Full::new(Bytes::new()))
            .unwrap();
        client
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .send()
            .await
    };

    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let connect = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .h2c()
        .send();
    let (connect, unrelated) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(connect, unrelated)
    })
    .await
    .expect("concurrent H2 requests timed out");
    let error = connect.unwrap_err();
    assert!(
        matches!(&error, aioduct::Error::Unsupported(message) if message.contains("requires status 200")),
        "{error}"
    );
    let unrelated = unrelated.unwrap();
    assert_eq!(unrelated.status(), http::StatusCode::OK);

    let next_request = Request::builder()
        .uri("/after")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client
            .forward(crate::valid_forward_request(next_request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .send(),
    )
    .await
    .expect("follow-up request did not recover on a fresh H2 connection")
    .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(unrelated.text().await.unwrap(), "unrelated body");
    server.await.unwrap();
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_ordinary_connect_rejects_http3_before_io() {
    let listener = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("example.com:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client
            .forward(crate::valid_forward_request(request))
            .upstream(
                format!("https://127.0.0.1:{}", addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_request(|parts| parts.version = http::Version::HTTP_3)
            .send(),
    )
    .await
    .expect("ordinary CONNECT validation must finish before HTTP/3 I/O")
    .unwrap_err();

    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("ordinary CONNECT")),
        "{error:?}"
    );
    let mut packet = [0u8; 1];
    assert_eq!(
        listener.recv_from(&mut packet).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "unsupported CONNECT must be rejected before sending a QUIC packet"
    );
}
