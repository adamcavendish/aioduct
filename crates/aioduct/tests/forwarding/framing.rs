use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aioduct::HttpEngineSend;
use aioduct::body::RequestBodySend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::TokioExec;
use bytes::Bytes;
use futures_util::stream;
use http::HeaderMap;
use http_body::Frame;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::client::conn::http2 as client_http2;
use hyper::server::conn::http1 as server_http1;
use hyper::server::conn::http2 as server_http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
enum ForwardProtocol {
    Auto,
    H2c,
    #[cfg(all(feature = "rustls", feature = "http3"))]
    Http3,
}

async fn start_h1_forwarder(
    client: HttpEngineSend<TokioRuntime, TcpConnector>,
    upstream: http::Uri,
    protocol: ForwardProtocol,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let client = client.clone();
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |request: Request<hyper::body::Incoming>| {
                            let client = client.clone();
                            let upstream = upstream.clone();
                            async move {
                                let mut forward = client
                                    .forward(crate::valid_forward_request(request))
                                    .upstream(upstream);
                                match protocol {
                                    ForwardProtocol::Auto => {}
                                    ForwardProtocol::H2c => forward = forward.h2c(),
                                    #[cfg(all(feature = "rustls", feature = "http3"))]
                                    ForwardProtocol::Http3 => {
                                        forward = forward.on_request(|parts| {
                                            parts.version = http::Version::HTTP_3;
                                        });
                                    }
                                }
                                let response: http::Response<RequestBodySend> =
                                    match forward.send().await {
                                        Ok(response) => response
                                            .into_http_response()
                                            .map(|body| body.boxed_unsync()),
                                        Err(error) => Response::builder()
                                            .status(http::StatusCode::BAD_GATEWAY)
                                            .body(
                                                Full::new(Bytes::from(error.to_string()))
                                                    .map_err(|never| match never {})
                                                    .boxed_unsync(),
                                            )
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

async fn start_h2_forwarder(
    client: HttpEngineSend<TokioRuntime, TcpConnector>,
    upstream: http::Uri,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let client = client.clone();
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |request: Request<hyper::body::Incoming>| {
                            let client = client.clone();
                            let upstream = upstream.clone();
                            async move {
                                let response: http::Response<RequestBodySend> =
                                    match client.forward(request).upstream(upstream).send().await {
                                        Ok(response) => response
                                            .into_http_response()
                                            .map(|body| body.boxed_unsync()),
                                        Err(error) => Response::builder()
                                            .status(http::StatusCode::BAD_GATEWAY)
                                            .body(
                                                Full::new(Bytes::from(error.to_string()))
                                                    .map_err(|never| match never {})
                                                    .boxed_unsync(),
                                            )
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

async fn exchange_h2(
    addr: SocketAddr,
    request: Request<Full<Bytes>>,
) -> Response<hyper::body::Incoming> {
    let stream = TcpStream::connect(addr).await.unwrap();
    let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
    let (mut sender, connection) = client_http2::Builder::new(TokioExec)
        .handshake(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    sender.send_request(request).await.unwrap()
}

async fn exchange_raw(addr: SocketAddr, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request).await.unwrap();
    stream.flush().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn successful_h1_connect_ignores_framing_without_losing_tunnel_bytes() {
    const GREETING: &[u8] = b"server tunnel bytes";
    const MESSAGE: &[u8] = b"client tunnel bytes";

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(
            request.starts_with(b"CONNECT target.example:443 HTTP/1.1\r\n"),
            "{}",
            String::from_utf8_lossy(&request),
        );

        let mut response = b"HTTP/1.1 200 Connection Established\r\nContent-Length: 999\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        response.extend_from_slice(GREETING);
        stream.write_all(&response).await.unwrap();
        stream.flush().await.unwrap();

        let mut message = [0_u8; MESSAGE.len()];
        stream.read_exact(&mut message).await.unwrap();
        assert_eq!(&message, MESSAGE);
        stream.write_all(&message).await.unwrap();
        stream.flush().await.unwrap();
    });

    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert!(
        !response
            .headers()
            .contains_key(http::header::CONTENT_LENGTH)
    );
    assert!(
        !response
            .headers()
            .contains_key(http::header::TRANSFER_ENCODING)
    );
    let mut tunnel = tokio::time::timeout(std::time::Duration::from_secs(2), response.upgrade())
        .await
        .expect("CONNECT tunnel handoff timed out")
        .unwrap();

    let mut greeting = [0_u8; GREETING.len()];
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tunnel.read_exact(&mut greeting),
    )
    .await
    .expect("immediate tunnel bytes timed out")
    .unwrap();
    assert_eq!(&greeting, GREETING);

    tunnel.write_all(MESSAGE).await.unwrap();
    tunnel.flush().await.unwrap();
    let mut echoed = [0_u8; MESSAGE.len()];
    tunnel.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, MESSAGE);
    server.await.unwrap();
}

#[tokio::test]
async fn h2_request_identical_content_lengths_normalize_before_h1_forwarding() {
    let (upstream_addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        let lengths = request
            .headers()
            .get_all(http::header::CONTENT_LENGTH)
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let body = request.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "lengths={lengths:?};body={}",
            String::from_utf8_lossy(&body),
        )))))
    })
    .await;
    let broker = start_h2_forwarder(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream_addr}").parse().unwrap(),
    )
    .await;
    let mut request = Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{broker}/upload"))
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::from_static(b"data")))
        .unwrap();
    request
        .headers_mut()
        .append(http::header::CONTENT_LENGTH, "4".parse().unwrap());
    request
        .headers_mut()
        .append(http::header::CONTENT_LENGTH, "004, 4".parse().unwrap());

    let response = exchange_h2(broker, request).await;
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"lengths=[\"4\"];body=data")
    );
}

#[tokio::test]
async fn invalid_h2_content_length_is_rejected_before_upstream_io() {
    for values in [&["3", "4"][..], &["+4"][..], &["4,"][..]] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut request = Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::from_static(b"data")))
            .unwrap();
        for value in values {
            request.headers_mut().append(
                http::header::CONTENT_LENGTH,
                http::HeaderValue::from_str(value).unwrap(),
            );
        }

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "invalid Content-Length {values:?} reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn post_hook_content_length_mismatch_is_rejected_before_h1_request_io() {
    let (upstream_addr, counter) = aioduct_test_server::h1::h1_server_with(|_request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"unexpected"))))
    })
    .await;
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .body(Full::new(Bytes::from_static(b"data")))
        .unwrap();

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
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
}

#[tokio::test]
async fn post_hook_content_length_mismatch_is_rejected_before_h2_request_io() {
    let (upstream_addr, counter) = aioduct_test_server::h2::h2_server_with(|_request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"unexpected"))))
    })
    .await;
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::from_static(b"data")))
        .unwrap();

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(request)
        .upstream(
            format!("http://{upstream_addr}")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
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
}

#[tokio::test]
async fn forwarded_h2_stream_enforces_post_hook_content_length_at_body_end() {
    for length in ["3", "5"] {
        let accepted = Arc::new(AtomicBool::new(false));
        let server_accepted = accepted.clone();
        let (upstream_addr, _) = aioduct_test_server::h2::h2_server_with(move |request| {
            let accepted = server_accepted.clone();
            async move {
                if request.into_body().collect().await.is_ok() {
                    accepted.store(true, Ordering::SeqCst);
                }
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"unexpected"))))
            }
        })
        .await;
        let body = StreamBody::new(stream::iter([Ok::<_, Infallible>(Frame::data(
            Bytes::from_static(b"data"),
        ))]));
        let request = Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .version(http::Version::HTTP_2)
            .body(body)
            .unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .on_request(move |parts| {
                parts.headers.insert(
                    http::header::CONTENT_LENGTH,
                    http::HeaderValue::from_static(length),
                );
            })
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::Hyper(_)), "{error}");
        tokio::task::yield_now().await;
        assert!(
            !accepted.load(Ordering::SeqCst),
            "upstream accepted a body that disagreed with Content-Length {length}"
        );
    }
}

#[tokio::test]
async fn response_hook_content_length_is_enforced_while_streaming_h2_body() {
    let (upstream_addr, _) = aioduct_test_server::h2::h2_server_with(|_request| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header(http::header::CONTENT_LENGTH, "4")
                .body(Full::new(Bytes::from_static(b"data")))
                .unwrap(),
        )
    })
    .await;

    for (length, expected) in [
        ("3", "exceeds Content-Length 3"),
        ("5", "ended after 4 bytes"),
    ] {
        let request = Request::builder()
            .method(http::Method::GET)
            .uri("/result")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .on_response(move |response| {
                response.headers_mut().insert(
                    http::header::CONTENT_LENGTH,
                    http::HeaderValue::from_static(length),
                );
            })
            .send()
            .await
            .unwrap();
        let error = response.bytes().await.unwrap_err();

        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[tokio::test]
async fn h2_response_identical_content_lengths_normalize_before_h1_forwarding() {
    let (upstream_addr, _) = aioduct_test_server::h2::h2_server_with(|_request| async move {
        let mut response = Response::new(Full::new(Bytes::from_static(b"data")));
        response
            .headers_mut()
            .append(http::header::CONTENT_LENGTH, "4".parse().unwrap());
        response
            .headers_mut()
            .append(http::header::CONTENT_LENGTH, "004, 4".parse().unwrap());
        Ok::<_, Infallible>(response)
    })
    .await;
    let broker = start_h1_forwarder(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream_addr}").parse().unwrap(),
        ForwardProtocol::H2c,
    )
    .await;
    let request = format!("GET /result HTTP/1.1\r\nHost: {broker}\r\nConnection: close\r\n\r\n");
    let response = String::from_utf8(exchange_raw(broker, request.as_bytes()).await).unwrap();
    let lengths = response
        .lines()
        .filter(|line| line.to_ascii_lowercase().starts_with("content-length:"))
        .collect::<Vec<_>>();

    assert_eq!(lengths, ["content-length: 4"]);
    assert!(response.ends_with("\r\n\r\ndata"), "{response}");
}

#[tokio::test]
async fn h1_request_rejects_non_chunked_transfer_codings_before_io() {
    for value in ["gzip", "gzip, chunked", "chunked, chunked"] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let request = Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "downstream.test")
            .header(http::header::TRANSFER_ENCODING, value)
            .body(Full::new(Bytes::from_static(b"data")))
            .unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();
        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "invalid Transfer-Encoding {value:?} reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn end_stream_forward_removes_trailer_declaration_on_h1_and_h2_wire() {
    let (h1_addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        let framing = format!(
            "trailer={};transfer-encoding={}",
            request.headers().contains_key(http::header::TRAILER),
            request
                .headers()
                .contains_key(http::header::TRANSFER_ENCODING)
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(framing))))
    })
    .await;
    let (h2_addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        let framing = format!(
            "trailer={};transfer-encoding={}",
            request.headers().contains_key(http::header::TRAILER),
            request
                .headers()
                .contains_key(http::header::TRANSFER_ENCODING)
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(framing))))
    })
    .await;

    for (upstream_addr, h2c) in [(h1_addr, false), (h2_addr, true)] {
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let request = Request::builder()
            .method(http::Method::POST)
            .uri("/empty-with-trailer-declaration")
            .header(http::header::HOST, "downstream.test")
            .header(http::header::TRAILER, "x-checksum")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let mut forward = client
            .forward(crate::valid_forward_request(request))
            .upstream(
                format!("http://{upstream_addr}")
                    .parse::<http::Uri>()
                    .unwrap(),
            );
        if h2c {
            forward = forward.h2c();
        }
        let response = forward.send().await.unwrap();
        assert_eq!(
            response.text().await.unwrap(),
            "trailer=false;transfer-encoding=false"
        );
    }
}

#[tokio::test]
async fn h1_response_rejects_non_chunked_transfer_codings_before_handoff() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n4\r\ndata\r\n0\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/result")
        .body(Full::new(Bytes::new()))
        .unwrap();

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
        "{error}"
    );
}

#[tokio::test]
async fn h1_chunked_response_ignores_removed_content_length_while_streaming() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 3\r\n\r\n4\r\ndata\r\n0\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/result")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert!(
        !response
            .headers()
            .contains_key(http::header::TRANSFER_ENCODING)
    );
    assert!(
        !response
            .headers()
            .contains_key(http::header::CONTENT_LENGTH)
    );
    assert_eq!(response.bytes().await.unwrap(), Bytes::from_static(b"data"));
}

#[tokio::test]
async fn forwarded_incoming_chunked_get_preserves_data_and_safe_trailers() {
    let (upstream_addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        assert_eq!(request.method(), http::Method::GET);
        let collected = request.into_body().collect().await.unwrap();
        let trailers = collected.trailers().unwrap();
        assert_eq!(trailers["x-checksum"], "sum");
        assert!(!trailers.contains_key("x-initial-secret"));
        assert_eq!(collected.to_bytes(), Bytes::from_static(b"data"));
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
    })
    .await;
    let broker = start_h1_forwarder(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream_addr}").parse().unwrap(),
        ForwardProtocol::Auto,
    )
    .await;

    let request = format!(
        "GET /stream HTTP/1.1\r\nHost: {broker}\r\nConnection: x-initial-secret, close\r\nX-Initial-Secret: header\r\nTransfer-Encoding: chunked\r\nTrailer: X-Checksum, X-Initial-Secret\r\n\r\n4\r\ndata\r\n0\r\nX-Checksum: sum\r\nX-Initial-Secret: trailer\r\n\r\n"
    );
    let response = exchange_raw(broker, request.as_bytes()).await;

    assert!(String::from_utf8_lossy(&response).contains("200 OK"));
    assert!(response.ends_with(b"ok"));
}

fn h2_body_with_trailers()
-> StreamBody<impl futures_core::Stream<Item = Result<Frame<Bytes>, Infallible>>> {
    let mut trailers = HeaderMap::new();
    trailers.insert("x-result-checksum", http::HeaderValue::from_static("sum"));
    StreamBody::new(stream::iter([
        Ok(Frame::data(Bytes::from_static(b"data"))),
        Ok(Frame::trailers(trailers)),
    ]))
}

fn h2_trailer_only_body(
    trailers: HeaderMap,
) -> StreamBody<impl futures_core::Stream<Item = Result<Frame<Bytes>, Infallible>>> {
    StreamBody::new(stream::iter([Ok(Frame::trailers(trailers))]))
}

async fn start_h2_trailer_only_upstream(
    status: http::StatusCode,
    trailers: HeaderMap,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let trailers = trailers.clone();
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |_request| {
                            let trailers = trailers.clone();
                            async move {
                                Ok::<_, Infallible>(
                                    Response::builder()
                                        .status(status)
                                        .body(h2_trailer_only_body(trailers))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    upstream_addr
}

async fn start_h2_forbidden_payload_upstream(status: http::StatusCode) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();
        let (_request, mut respond) = connection.accept().await.unwrap().unwrap();
        let mut body = respond
            .send_response(Response::builder().status(status).body(()).unwrap(), false)
            .unwrap();
        body.send_data(Bytes::from_static(b"forbidden"), true)
            .unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), connection.accept()).await;
    });
    upstream_addr
}

#[tokio::test]
async fn forward_h2_rejects_head_and_205_payloads_before_handoff() {
    for (method, status) in [
        (http::Method::HEAD, http::StatusCode::OK),
        (http::Method::GET, http::StatusCode::RESET_CONTENT),
    ] {
        let upstream_addr = start_h2_forbidden_payload_upstream(status).await;
        let request = Request::builder()
            .method(method.clone())
            .uri("/forbidden-payload")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "downstream.test")
            .body(Full::new(Bytes::new()))
            .unwrap();

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
            matches!(
                error,
                aioduct::Error::Hyper(_) | aioduct::Error::InvalidHeader(_)
            ),
            "unexpected {method} {status} error: {error}"
        );
    }
}

#[tokio::test]
async fn forward_h2_rejects_204_and_304_trailers_for_every_downstream_version() {
    for status in [http::StatusCode::NO_CONTENT, http::StatusCode::NOT_MODIFIED] {
        let mut trailers = HeaderMap::new();
        trailers.insert("x-result", http::HeaderValue::from_static("must-reject"));
        let upstream_addr = start_h2_trailer_only_upstream(status, trailers).await;
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

        for downstream_version in [
            http::Version::HTTP_10,
            http::Version::HTTP_11,
            http::Version::HTTP_2,
            http::Version::HTTP_3,
        ] {
            let mut request = Request::builder()
                .method(http::Method::GET)
                .uri("/bodyless-trailers")
                .version(downstream_version)
                .body(Full::new(Bytes::new()))
                .unwrap();
            if matches!(
                downstream_version,
                http::Version::HTTP_10 | http::Version::HTTP_11
            ) {
                request.headers_mut().insert(
                    http::header::HOST,
                    http::HeaderValue::from_static("downstream.test"),
                );
            }

            let error = client
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

            if downstream_version == http::Version::HTTP_3 {
                assert!(
                    error.to_string().contains("HTTP/3 response trailers"),
                    "unexpected {downstream_version:?} {status} trailer error: {error}"
                );
            } else {
                assert!(
                    error.to_string().contains("must not contain trailers"),
                    "unexpected {downstream_version:?} {status} trailer error: {error}"
                );
            }
        }
    }
}

#[tokio::test]
async fn forward_h2_204_and_304_trailers_fail_before_h1_response_handoff() {
    for status in [http::StatusCode::NO_CONTENT, http::StatusCode::NOT_MODIFIED] {
        let mut trailers = HeaderMap::new();
        trailers.insert("x-result", http::HeaderValue::from_static("must-reject"));
        let upstream_addr = start_h2_trailer_only_upstream(status, trailers).await;
        let broker = start_h1_forwarder(
            HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
            format!("http://{upstream_addr}").parse().unwrap(),
            ForwardProtocol::H2c,
        )
        .await;

        for wire_version in ["HTTP/1.0", "HTTP/1.1"] {
            let request = format!(
                "GET /bodyless-trailers {wire_version}\r\nHost: {broker}\r\nConnection: close\r\n\r\n"
            );
            let response = String::from_utf8(exchange_raw(broker, request.as_bytes()).await)
                .expect("broker response must be valid UTF-8");
            let status_line = response.lines().next().unwrap_or_default();
            assert!(
                status_line.contains(" 502 "),
                "{wire_version} downstream silently received {status}: {response}"
            );
            assert!(response.contains("must not contain trailers"), "{response}");
        }
    }
}

#[tokio::test]
async fn forward_h2_rejects_te_in_response_trailers_for_every_downstream_version() {
    let mut trailers = HeaderMap::new();
    trailers.insert(http::header::TE, http::HeaderValue::from_static("trailers"));
    trailers.insert(
        "x-result",
        http::HeaderValue::from_static("must-not-reach-client"),
    );
    let upstream_addr = start_h2_trailer_only_upstream(http::StatusCode::OK, trailers).await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    for downstream_version in [
        http::Version::HTTP_10,
        http::Version::HTTP_11,
        http::Version::HTTP_2,
        http::Version::HTTP_3,
    ] {
        let mut request = Request::builder()
            .method(http::Method::GET)
            .uri("/response-trailer-te")
            .version(downstream_version)
            .body(Full::new(Bytes::new()))
            .unwrap();
        if matches!(
            downstream_version,
            http::Version::HTTP_10 | http::Version::HTTP_11
        ) {
            request.headers_mut().insert(
                http::header::HOST,
                http::HeaderValue::from_static("downstream.test"),
            );
        }

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
        let error = response.into_body().collect().await.unwrap_err();

        if downstream_version == http::Version::HTTP_3 {
            assert!(
                error.to_string().contains("HTTP/3 response trailers"),
                "unexpected {downstream_version:?} response trailer error: {error}"
            );
        } else {
            assert!(
                error
                    .to_string()
                    .contains("trailers contain forbidden field `te`"),
                "unexpected {downstream_version:?} response trailer error: {error}"
            );
        }
    }
}

#[tokio::test]
async fn forward_h2_rejects_forbidden_response_content_lengths_before_handoff() {
    for (status, length) in [
        (http::StatusCode::NO_CONTENT, "0"),
        (http::StatusCode::RESET_CONTENT, "7"),
    ] {
        let (upstream_addr, _) =
            aioduct_test_server::h2::h2_server_with(move |_request| async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(status)
                        .header(http::header::CONTENT_LENGTH, length)
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
            })
            .await;
        let request = Request::builder()
            .method(http::Method::GET)
            .uri("/invalid-length")
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();

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
            matches!(
                error,
                aioduct::Error::InvalidHeader(_) | aioduct::Error::Hyper(_)
            ),
            "{error}"
        );
    }
}

async fn start_h2_trailer_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(|_request| async move {
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .header(http::header::CONTENT_LENGTH, "4")
                                    .header(http::header::TRAILER, "x-result-checksum")
                                    .body(h2_body_with_trailers())
                                    .unwrap(),
                            )
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

async fn start_h2_undeclared_trailer_upstream(known_length: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(
                        io,
                        service_fn(move |_request| async move {
                            let mut response = Response::builder();
                            if known_length {
                                response = response.header(http::header::CONTENT_LENGTH, "4");
                            }
                            Ok::<_, Infallible>(response.body(h2_body_with_trailers()).unwrap())
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

async fn h2_response_forwarded_to_h1(version: &str, accept_trailers: bool) -> String {
    let upstream_addr = start_h2_trailer_upstream().await;
    let broker = start_h1_forwarder(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream_addr}").parse().unwrap(),
        ForwardProtocol::H2c,
    )
    .await;
    let trailer_headers = if accept_trailers {
        "Connection: te, close\r\nTE: trailers\r\n"
    } else {
        "Connection: close\r\n"
    };
    let request = format!("GET /result {version}\r\nHost: {broker}\r\n{trailer_headers}\r\n");
    String::from_utf8(exchange_raw(broker, request.as_bytes()).await).unwrap()
}

#[tokio::test]
async fn h2_response_with_content_length_and_trailers_is_chunked_for_h11() {
    let response = h2_response_forwarded_to_h1("HTTP/1.1", true).await;
    let lower = response.to_ascii_lowercase();

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        lower.contains("\r\ntransfer-encoding: chunked\r\n"),
        "{response}"
    );
    assert!(!lower.contains("\r\ncontent-length:"), "{response}");
    assert!(
        lower.contains("\r\ntrailer: x-result-checksum\r\n"),
        "{response}"
    );
    assert!(
        lower.contains("\r\n4\r\ndata\r\n0\r\nx-result-checksum: sum\r\n\r\n"),
        "{response}"
    );
}

#[tokio::test]
async fn h2_response_trailers_are_removed_for_h10() {
    let response = h2_response_forwarded_to_h1("HTTP/1.0", false).await;
    let lower = response.to_ascii_lowercase();

    assert!(response.starts_with("HTTP/1.0 200"), "{response}");
    assert!(lower.contains("\r\ncontent-length: 4\r\n"), "{response}");
    assert!(!lower.contains("transfer-encoding"), "{response}");
    assert!(!lower.contains("trailer:"), "{response}");
    assert!(!lower.contains("x-result-checksum"), "{response}");
    assert!(response.ends_with("\r\n\r\ndata"), "{response}");
}

#[tokio::test]
async fn unknown_length_h2_response_is_close_delimited_for_h10() {
    let upstream_addr = start_h2_undeclared_trailer_upstream(false).await;
    let broker = start_h1_forwarder(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream_addr}").parse().unwrap(),
        ForwardProtocol::H2c,
    )
    .await;
    let request =
        format!("GET /result HTTP/1.0\r\nHost: {broker}\r\nConnection: keep-alive\r\n\r\n");
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        exchange_raw(broker, request.as_bytes()),
    )
    .await
    .expect("close-delimited HTTP/1.0 response did not close");
    let response = String::from_utf8(response).unwrap();
    let lower = response.to_ascii_lowercase();

    assert!(response.starts_with("HTTP/1.0 200"), "{response}");
    assert!(!lower.contains("\r\ncontent-length:"), "{response}");
    assert!(!lower.contains("\r\ntransfer-encoding:"), "{response}");
    assert!(!lower.contains("\r\ntrailer:"), "{response}");
    assert!(!lower.contains("x-result-checksum"), "{response}");
    assert!(response.ends_with("\r\n\r\ndata"), "{response}");
}

#[tokio::test]
async fn h2_response_trailers_are_removed_when_h11_client_did_not_accept_them() {
    let response = h2_response_forwarded_to_h1("HTTP/1.1", false).await;
    let lower = response.to_ascii_lowercase();

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(lower.contains("\r\ncontent-length: 4\r\n"), "{response}");
    assert!(!lower.contains("trailer:"), "{response}");
    assert!(!lower.contains("x-result-checksum"), "{response}");
    assert!(response.ends_with("\r\n\r\ndata"), "{response}");
}

#[tokio::test]
async fn undeclared_h2_trailers_are_not_forwarded_to_h11_with_fixed_length() {
    let upstream_addr = start_h2_undeclared_trailer_upstream(true).await;
    let broker = start_h1_forwarder(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream_addr}").parse().unwrap(),
        ForwardProtocol::H2c,
    )
    .await;
    let request = format!(
        "GET /result HTTP/1.1\r\nHost: {broker}\r\nConnection: te, close\r\nTE: trailers\r\n\r\n"
    );
    let response = String::from_utf8(exchange_raw(broker, request.as_bytes()).await).unwrap();
    let lower = response.to_ascii_lowercase();

    assert!(lower.contains("\r\ncontent-length: 4\r\n"), "{response}");
    assert!(!lower.contains("\r\ntrailer:"), "{response}");
    assert!(!lower.contains("x-result-checksum"), "{response}");
    assert!(response.ends_with("\r\n\r\ndata"), "{response}");
}

#[tokio::test]
async fn undeclared_h2_trailers_are_not_forwarded_to_h11_chunked_response() {
    let upstream_addr = start_h2_undeclared_trailer_upstream(false).await;
    let broker = start_h1_forwarder(
        HttpEngineSend::<TokioRuntime, TcpConnector>::new(),
        format!("http://{upstream_addr}").parse().unwrap(),
        ForwardProtocol::H2c,
    )
    .await;
    let request = format!(
        "GET /result HTTP/1.1\r\nHost: {broker}\r\nConnection: te, close\r\nTE: trailers\r\n\r\n"
    );
    let response = String::from_utf8(exchange_raw(broker, request.as_bytes()).await).unwrap();
    let lower = response.to_ascii_lowercase();

    assert!(
        lower.contains("\r\ntransfer-encoding: chunked\r\n"),
        "{response}"
    );
    assert!(!lower.contains("\r\ntrailer:"), "{response}");
    assert!(!lower.contains("x-result-checksum"), "{response}");
    assert!(
        response.ends_with("\r\n4\r\ndata\r\n0\r\n\r\n"),
        "{response}"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn h3_response_is_translated_to_h11_before_downstream_dispatch() {
    let (upstream_addr, _, _) = aioduct_test_server::h3::h3_server_with(|_request, _body| {
        (http::StatusCode::OK, Bytes::from_static(b"h3-data"))
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let broker = start_h1_forwarder(
        client,
        format!("https://127.0.0.1:{}", upstream_addr.port())
            .parse()
            .unwrap(),
        ForwardProtocol::Http3,
    )
    .await;
    let request = format!("GET /result HTTP/1.1\r\nHost: {broker}\r\nConnection: close\r\n\r\n");
    let response = String::from_utf8(exchange_raw(broker, request.as_bytes()).await).unwrap();

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("\r\n7\r\nh3-data\r\n0\r\n\r\n"),
        "{response}"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_h3_rejects_head_and_205_payloads_before_handoff() {
    for (method, status) in [
        (http::Method::HEAD, http::StatusCode::OK),
        (http::Method::GET, http::StatusCode::RESET_CONTENT),
    ] {
        let (upstream_addr, _, _) =
            aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| async move {
                while matches!(stream.recv_data().await, Ok(Some(_))) {}
                let _ = stream.recv_trailers().await;
                stream
                    .send_response(Response::builder().status(status).body(()).unwrap())
                    .await
                    .unwrap();
                stream
                    .send_data(Bytes::from_static(b"forbidden"))
                    .await
                    .unwrap();
                stream.finish().await.unwrap();
            })
            .await;
        let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
            .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
            .http3(true)
            .unwrap()
            .build()
            .unwrap();
        let request = Request::builder()
            .method(method.clone())
            .uri("/forbidden-payload")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "downstream.test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = client
            .forward(request)
            .upstream(
                format!("https://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_request(|parts| parts.version = http::Version::HTTP_3)
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                aioduct::Error::InvalidHeader(ref message)
                    if message.contains("payload frame for a response that cannot contain a body")
                        || message.contains("exceeds Content-Length 0")
            ),
            "unexpected H3 {method} {status} error: {error}"
        );
    }
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn unknown_length_h3_response_is_close_delimited_for_h10() {
    let (upstream_addr, _, _) = aioduct_test_server::h3::h3_server_with(|_request, _body| {
        (http::StatusCode::OK, Bytes::from_static(b"h3-data"))
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let broker = start_h1_forwarder(
        client,
        format!("https://127.0.0.1:{}", upstream_addr.port())
            .parse()
            .unwrap(),
        ForwardProtocol::Http3,
    )
    .await;
    let request =
        format!("GET /result HTTP/1.0\r\nHost: {broker}\r\nConnection: keep-alive\r\n\r\n");
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        exchange_raw(broker, request.as_bytes()),
    )
    .await
    .expect("close-delimited HTTP/1.0 response did not close");
    let response = String::from_utf8(response).unwrap();
    let lower = response.to_ascii_lowercase();

    assert!(response.starts_with("HTTP/1.0 200"), "{response}");
    assert!(!lower.contains("\r\ncontent-length:"), "{response}");
    assert!(!lower.contains("\r\ntransfer-encoding:"), "{response}");
    assert!(response.ends_with("\r\n\r\nh3-data"), "{response}");
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn h3_trailers_fail_before_h11_response_handoff() {
    let (upstream_addr, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            while matches!(stream.recv_data().await, Ok(Some(_))) {}
            let _ = stream.recv_trailers().await;
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            stream
                .send_data(Bytes::from_static(b"h3-data"))
                .await
                .unwrap();
            let mut trailers = HeaderMap::new();
            trailers.insert("x-result-checksum", http::HeaderValue::from_static("sum"));
            stream.send_trailers(trailers).await.unwrap();
        })
        .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let broker = start_h1_forwarder(
        client,
        format!("https://127.0.0.1:{}", upstream_addr.port())
            .parse()
            .unwrap(),
        ForwardProtocol::Http3,
    )
    .await;
    let request = format!(
        "GET /result HTTP/1.1\r\nHost: {broker}\r\nConnection: te, close\r\nTE: trailers\r\n\r\n"
    );
    let response = String::from_utf8(exchange_raw(broker, request.as_bytes()).await).unwrap();
    assert!(
        response.is_empty(),
        "unsupported H3 trailers leaked a partial H1 response: {response}"
    );
}
