use std::convert::Infallible;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use bytes::Bytes;
use futures_util::stream;
use http::HeaderMap;
use http_body::Frame;
use http_body_util::{BodyExt, StreamBody};
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct TrailerCase {
    name: &'static str,
    request_allowed: bool,
    response_allowed: bool,
}

const TRAILER_CASES: &[TrailerCase] = &[
    TrailerCase {
        name: "allow",
        request_allowed: false,
        response_allowed: false,
    },
    TrailerCase {
        name: "content-language",
        request_allowed: false,
        response_allowed: false,
    },
    TrailerCase {
        name: "last-modified",
        request_allowed: false,
        response_allowed: false,
    },
    TrailerCase {
        name: "accept-ranges",
        request_allowed: false,
        response_allowed: true,
    },
    TrailerCase {
        name: "authentication-info",
        request_allowed: false,
        response_allowed: true,
    },
    TrailerCase {
        name: "etag",
        request_allowed: false,
        response_allowed: true,
    },
    TrailerCase {
        name: "expires",
        request_allowed: false,
        response_allowed: false,
    },
    TrailerCase {
        name: "proxy-authentication-info",
        request_allowed: false,
        response_allowed: true,
    },
    TrailerCase {
        name: "x-safe-extension",
        request_allowed: true,
        response_allowed: true,
    },
];

fn body_with_single_trailer(
    name: &'static str,
) -> StreamBody<impl futures_core::Stream<Item = Result<Frame<Bytes>, Infallible>>> {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        http::HeaderName::from_static(name),
        http::HeaderValue::from_static("value"),
    );
    StreamBody::new(stream::iter([
        Ok(Frame::data(Bytes::from_static(b"body"))),
        Ok(Frame::trailers(trailers)),
    ]))
}

fn body_with_trailers(
    data: &'static [u8],
    trailer_name: &'static str,
    trailer_value: &'static str,
) -> StreamBody<impl futures_core::Stream<Item = Result<Frame<Bytes>, Infallible>>> {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        http::header::HeaderName::from_static(trailer_name),
        http::header::HeaderValue::from_static(trailer_value),
    );
    trailers.insert(
        http::header::CONNECTION,
        http::HeaderValue::from_static("x-hop-secret"),
    );
    trailers.insert(
        "x-hop-secret",
        http::HeaderValue::from_static("must-not-forward"),
    );
    trailers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer must-not-forward"),
    );
    trailers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/must-not-forward"),
    );
    StreamBody::new(stream::iter([
        Ok(Frame::data(Bytes::from_static(data))),
        Ok(Frame::trailers(trailers)),
    ]))
}

async fn h2_response_server_with_trailer(name: &'static str) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(move |_request| async move {
            let mut trailers = HeaderMap::new();
            trailers.insert(
                http::HeaderName::from_static(name),
                http::HeaderValue::from_static("value"),
            );
            let body = StreamBody::new(stream::iter([
                Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"body"))),
                Ok(Frame::trailers(trailers)),
            ]));
            Ok::<_, Infallible>(Response::new(body))
        });
        let _ = hyper::server::conn::http2::Builder::new(aioduct_test_server::TokioExec)
            .serve_connection(aioduct_test_server::TokioIo::new(stream), service)
            .await;
    });
    addr
}

fn error_chain_contains(error: &(dyn std::error::Error + 'static), expected: &str) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error.to_string().contains(expected) {
            return true;
        }
        current = error.source();
    }
    false
}

#[tokio::test]
async fn forward_h1_request_preserves_trailer_declaration_and_frames() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        let declaration = request
            .headers()
            .get(http::header::TRAILER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing")
            .to_owned();
        let collected = request.into_body().collect().await.unwrap();
        let trailers = collected.trailers().unwrap();
        assert!(!trailers.contains_key(http::header::CONNECTION));
        assert!(!trailers.contains_key("x-hop-secret"));
        assert!(!trailers.contains_key(http::header::AUTHORIZATION));
        assert!(!trailers.contains_key(http::header::CONTENT_TYPE));
        let trailer = collected
            .trailers()
            .and_then(|trailers| trailers.get("x-upload-checksum"))
            .and_then(|value| value.to_str().ok())
            .unwrap_or("missing")
            .to_owned();
        let data = String::from_utf8(collected.to_bytes().to_vec()).unwrap();
        Ok::<_, Infallible>(Response::new(http_body_util::Full::new(Bytes::from(
            format!("declaration={declaration};trailer={trailer};body={data}"),
        ))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .header(
            http::header::TRAILER,
            "x-upload-checksum, Authorization, Content-Type, x-hop-secret",
        )
        .header(http::header::CONNECTION, "x-hop-secret")
        .body(body_with_trailers(
            b"forwarded-body",
            "x-upload-checksum",
            "sha256:request",
        ))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        "declaration=x-upload-checksum;trailer=sha256:request;body=forwarded-body"
    );
}

#[tokio::test]
async fn forward_h1_request_drops_undeclared_trailer_frames() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        let collected = request.into_body().collect().await.unwrap();
        assert!(collected.trailers().is_none());
        assert_eq!(collected.to_bytes(), Bytes::from_static(b"forwarded-body"));
        Ok::<_, Infallible>(Response::new(http_body_util::Full::new(
            Bytes::from_static(b"h1-ok"),
        )))
    })
    .await;
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .body(body_with_trailers(
            b"forwarded-body",
            "x-upload-checksum",
            "sha256:h1-undeclared",
        ))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "h1-ok");
}

#[tokio::test]
async fn forward_h1_response_preserves_trailer_declaration_and_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0, "client closed before completing request headers");
            request.extend_from_slice(&buffer[..read]);
        }
        let request_headers = String::from_utf8(request).unwrap();
        assert!(request_headers.lines().any(|line| {
            line.eq_ignore_ascii_case("connection: TE")
                || line.eq_ignore_ascii_case("connection:TE")
        }));
        assert!(request_headers.lines().any(|line| {
            line.eq_ignore_ascii_case("te: trailers") || line.eq_ignore_ascii_case("te:trailers")
        }));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: x-hop-secret\r\nX-Hop-Secret: initial-secret\r\nTrailer: X-Result-Checksum, X-Hop-Secret, Set-Cookie, Content-Type\r\n\r\nd\r\nupstream-body\r\n0\r\nX-Hop-Secret: must-not-forward\r\nSet-Cookie: must-not-forward=true\r\nContent-Type: application/must-not-forward\r\nX-Result-Checksum: sha256:response\r\n\r\n",
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/result")
        .header(http::header::CONNECTION, "te")
        .header(http::header::TE, "trailers")
        .body(http_body_util::Full::new(Bytes::new()))
        .unwrap();
    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert!(
        response
            .headers()
            .get(http::header::TRAILER)
            .unwrap()
            .to_str()
            .unwrap()
            .eq_ignore_ascii_case("x-result-checksum")
    );
    assert!(!response.headers().contains_key("x-hop-secret"));
    let collected = response.into_body().collect().await.unwrap();
    assert!(
        !collected
            .trailers()
            .unwrap()
            .contains_key(http::header::CONNECTION)
    );
    assert!(!collected.trailers().unwrap().contains_key("x-hop-secret"));
    assert!(
        !collected
            .trailers()
            .unwrap()
            .contains_key(http::header::SET_COOKIE)
    );
    assert!(
        !collected
            .trailers()
            .unwrap()
            .contains_key(http::header::CONTENT_TYPE)
    );
    assert_eq!(
        collected
            .trailers()
            .and_then(|trailers| trailers.get("x-result-checksum"))
            .unwrap(),
        "sha256:response"
    );
    assert_eq!(collected.to_bytes(), Bytes::from_static(b"upstream-body"));
}

#[tokio::test]
async fn forward_h1_response_to_h2_uses_directional_trailer_policy() {
    let downstream_version = http::Version::HTTP_2;
    for case in TRAILER_CASES {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let name = case.name;
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0, "client closed before request headers");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: {name}\r\n\r\n4\r\nbody\r\n0\r\n{name}: value\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
        });
        let request = Request::builder()
            .method(http::Method::GET)
            .uri("/response")
            .version(downstream_version)
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();

        let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap();
        let collected = response.into_body().collect().await.unwrap();
        let has_trailer = collected
            .trailers()
            .is_some_and(|trailers| trailers.contains_key(case.name));

        assert_eq!(collected.to_bytes(), Bytes::from_static(b"body"));
        assert_eq!(
            has_trailer, case.response_allowed,
            "unexpected H1-to-{downstream_version:?} response trailer result for {}",
            case.name
        );
    }
}

#[tokio::test]
async fn forward_h1_response_trailers_to_h3_fail_closed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0, "client closed before request headers");
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: x-checksum\r\n\r\n4\r\nbody\r\n0\r\nx-checksum: value\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/response")
        .version(http::Version::HTTP_3)
        .body(http_body_util::Full::new(Bytes::new()))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();
    let error = response.into_body().collect().await.unwrap_err();
    assert!(error_chain_contains(&error, "HTTP/3 response trailers"));
}

#[tokio::test]
async fn forward_native_h2_response_uses_directional_trailer_policy() {
    for case in TRAILER_CASES {
        let addr = h2_response_server_with_trailer(case.name).await;
        let request = Request::builder()
            .method(http::Method::GET)
            .uri("/response")
            .version(http::Version::HTTP_2)
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();

        let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .send()
            .await
            .unwrap();
        let result = response.into_body().collect().await;

        if case.response_allowed {
            let collected = result.unwrap();
            assert!(
                collected
                    .trailers()
                    .is_some_and(|trailers| trailers.contains_key(case.name)),
                "native H2 response trailer {} was lost",
                case.name
            );
            assert_eq!(collected.to_bytes(), Bytes::from_static(b"body"));
        } else {
            let error = result.unwrap_err();
            assert!(
                error_chain_contains(&error, case.name),
                "unexpected native H2 response trailer error for {}: {error:?}",
                case.name
            );
        }
    }
}

#[tokio::test]
async fn forward_h1_request_to_h2_strips_forbidden_trailer_fields() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        let collected = request.into_body().collect().await.unwrap();
        let trailers = collected.trailers().unwrap();
        assert_eq!(trailers["x-upload-checksum"], "sha256:h2");
        assert!(!trailers.contains_key(http::header::AUTHORIZATION));
        assert!(!trailers.contains_key(http::header::CONTENT_TYPE));
        assert!(!trailers.contains_key(http::header::CONNECTION));
        assert!(!trailers.contains_key("x-hop-secret"));
        Ok::<_, Infallible>(Response::new(http_body_util::Full::new(
            Bytes::from_static(b"h2-ok"),
        )))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .header(
            http::header::TRAILER,
            "x-upload-checksum, Authorization, Content-Type",
        )
        .body(body_with_trailers(
            b"forwarded-body",
            "x-upload-checksum",
            "sha256:h2",
        ))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "h2-ok");
}

#[tokio::test]
async fn forward_h1_request_to_h2_uses_directional_trailer_policy() {
    for case in TRAILER_CASES {
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let observed_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(observed_tx)));
        let server_tx = observed_tx.clone();
        let (addr, _) = aioduct_test_server::h2::h2_server_with(move |request| {
            let observed_tx = server_tx.clone();
            async move {
                let collected = request.into_body().collect().await.unwrap();
                if let Some(sender) = observed_tx.lock().unwrap().take() {
                    sender.send(collected.trailers().cloned()).unwrap();
                }
                Ok::<_, Infallible>(Response::new(http_body_util::Full::new(
                    Bytes::from_static(b"h2-ok"),
                )))
            }
        })
        .await;
        let request = Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .header(http::header::TRAILER, case.name)
            .body(body_with_single_trailer(case.name))
            .unwrap();

        let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .send()
            .await
            .unwrap();

        assert_eq!(response.text().await.unwrap(), "h2-ok");
        let observed = observed_rx.await.unwrap();
        assert_eq!(
            observed
                .as_ref()
                .is_some_and(|trailers| trailers.contains_key(case.name)),
            case.request_allowed,
            "unexpected H1-to-H2 request trailer result for {}",
            case.name
        );
    }
}

#[tokio::test]
async fn forward_native_h2_and_h3_requests_reject_connection_specific_trailers() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        let _ = request.into_body().collect().await;
        Ok::<_, Infallible>(Response::new(http_body_util::Full::new(
            Bytes::from_static(b"unexpected"),
        )))
    })
    .await;

    for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
        let request = Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .version(version)
            .body(body_with_trailers(
                b"forwarded-body",
                "x-upload-checksum",
                "sha256:native",
            ))
            .unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();

        if version == http::Version::HTTP_3 {
            assert!(
                error_chain_contains(&error, "HTTP/3 request trailers"),
                "unexpected native {version:?} trailer error: {error:?}"
            );
        } else {
            assert!(
                error_chain_contains(&error, "connection-specific field"),
                "unexpected native {version:?} trailer error: {error:?}"
            );
        }
    }
}

#[tokio::test]
async fn forward_native_h2_request_trailer_policy_is_direction_aware() {
    let version = http::Version::HTTP_2;
    for case in TRAILER_CASES {
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let observed_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(observed_tx)));
        let server_tx = observed_tx.clone();
        let (addr, _) = aioduct_test_server::h1::h1_server_with(move |request| {
            let observed_tx = server_tx.clone();
            async move {
                let observed = request
                    .into_body()
                    .collect()
                    .await
                    .ok()
                    .and_then(|collected| collected.trailers().cloned());
                if let Some(sender) = observed_tx.lock().unwrap().take() {
                    let _ = sender.send(observed);
                }
                Ok::<_, Infallible>(Response::new(http_body_util::Full::new(
                    Bytes::from_static(b"upstream-finished"),
                )))
            }
        })
        .await;
        let request = Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .version(version)
            .header(http::header::TRAILER, case.name)
            .body(body_with_single_trailer(case.name))
            .unwrap();

        let result = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await;

        if case.request_allowed {
            result.unwrap();
            let observed = tokio::time::timeout(std::time::Duration::from_secs(1), observed_rx)
                .await
                .expect("upstream did not receive the safe forwarded trailer")
                .unwrap();
            assert!(
                observed
                    .as_ref()
                    .is_some_and(|trailers| trailers.contains_key(case.name)),
                "safe native {version:?} trailer {} did not reach upstream",
                case.name
            );
        } else {
            let error = result.unwrap_err();
            assert!(
                error_chain_contains(&error, case.name),
                "unexpected native {version:?} trailer error for {}: {error:?}",
                case.name
            );
            if let Ok(Ok(observed)) =
                tokio::time::timeout(std::time::Duration::from_millis(100), observed_rx).await
            {
                assert!(
                    observed
                        .as_ref()
                        .is_none_or(|trailers| !trailers.contains_key(case.name)),
                    "invalid native {version:?} trailer {} reached upstream",
                    case.name
                );
            }
        }
    }
}

#[tokio::test]
async fn forward_native_h3_request_trailers_fail_closed() {
    for name in ["x-safe-extension", "authorization"] {
        let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
            let _ = request.into_body().collect().await;
            Ok::<_, Infallible>(Response::new(http_body_util::Full::new(
                Bytes::from_static(b"unexpected"),
            )))
        })
        .await;
        let request = Request::builder()
            .method(http::Method::POST)
            .uri("/upload")
            .version(http::Version::HTTP_3)
            .body(body_with_single_trailer(name))
            .unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();

        assert!(
            error_chain_contains(&error, "HTTP/3 request trailers"),
            "unexpected native H3 trailer error for {name}: {error:?}"
        );
    }
}

#[tokio::test]
async fn forward_native_h2_and_h3_reject_http2_settings_before_upstream_io() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
        let request = Request::builder()
            .method(http::Method::GET)
            .uri("/headers")
            .version(version)
            .header("http2-settings", "settings")
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();
        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();

        assert!(
            error_chain_contains(&error, "connection-specific field `http2-settings`"),
            "unexpected native {version:?} header error: {error:?}"
        );
    }

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "invalid native header opened an upstream connection"
    );
}

#[tokio::test]
async fn forward_h2_request_preserves_safe_undeclared_trailers() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        let collected = request.into_body().collect().await.unwrap();
        let trailers = collected.trailers().unwrap();
        assert_eq!(trailers["x-upload-checksum"], "sha256:h2-undeclared");
        assert!(!trailers.contains_key(http::header::AUTHORIZATION));
        Ok::<_, Infallible>(Response::new(http_body_util::Full::new(
            Bytes::from_static(b"h2-ok"),
        )))
    })
    .await;
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .body(body_with_trailers(
            b"forwarded-body",
            "x-upload-checksum",
            "sha256:h2-undeclared",
        ))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "h2-ok");
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_request_trailers_to_h3_fail_closed() {
    let (addr, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            while matches!(stream.recv_data().await, Ok(Some(_))) {}
            let _ = stream.recv_trailers().await;
        })
        .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .header(http::header::TRAILER, "x-upload-checksum")
        .body(body_with_trailers(
            b"forwarded-body",
            "x-upload-checksum",
            "sha256:h3",
        ))
        .unwrap();

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("request trailers")),
        "{error:?}"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test(flavor = "current_thread")]
async fn forward_request_trailers_to_h3_fail_closed_when_response_races_them() {
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let release_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(release_tx)));
    let server_release = release_tx.clone();
    let (addr, _, _) = aioduct_test_server::h3::h3_server_streaming(move |_request, mut stream| {
        let release_tx = server_release.clone();
        async move {
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            if let Some(sender) = release_tx.lock().unwrap().take() {
                let _ = sender.send(());
            }
            stream.finish().await.unwrap();
        }
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let mut trailers = HeaderMap::new();
    trailers.insert("x-upload-checksum", "complete".parse().unwrap());
    let body = StreamBody::new(stream::once(async move {
        release_rx.await.unwrap();
        Ok::<_, aioduct::Error>(Frame::trailers(trailers))
    }));
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .header(http::header::TRAILER, "x-upload-checksum")
        .body(body)
        .unwrap();

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("request trailers")),
        "{error:?}"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_forbidden_only_request_trailer_to_h3_fails_closed_before_sanitization() {
    let (addr, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            while matches!(stream.recv_data().await, Ok(Some(_))) {}
        })
        .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/upload")
        .header(http::header::TRAILER, "authorization")
        .body(body_with_single_trailer("authorization"))
        .unwrap();

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("request trailers")),
        "{error:?}"
    );
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_response_trailers_from_h3_fail_closed() {
    let (addr, _, _) =
        aioduct_test_server::h3::h3_server_streaming(|_request, mut stream| async move {
            while matches!(stream.recv_data().await, Ok(Some(_))) {}
            let _ = stream.recv_trailers().await;
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            stream.send_data(Bytes::from_static(b"body")).await.unwrap();
            let mut trailers = HeaderMap::new();
            trailers.insert("x-checksum", http::HeaderValue::from_static("value"));
            stream.send_trailers(trailers).await.unwrap();
        })
        .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/response")
        .version(http::Version::HTTP_3)
        .body(http_body_util::Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(request)
        .upstream(
            format!("https://127.0.0.1:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap();
    let error = response.into_body().collect().await.unwrap_err();
    assert!(
        error_chain_contains(&error, "response trailers"),
        "{error:?}"
    );
}
