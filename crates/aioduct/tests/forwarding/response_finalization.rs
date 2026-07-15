use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::runtime::{ConnectorSend, TokioRuntime};
use aioduct::{
    CONTENT_DIGEST, Error, HttpEngineSend, MessageSignatureBase, MessageSignatureComponent,
    MessageSignatureConfig, MessageSignatureError, sha256_content_digest_value,
};
use bytes::Bytes;
use http::header::HeaderName;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

fn unused_signature(_: &[u8]) -> Result<Vec<u8>, MessageSignatureError> {
    Ok(b"unused".to_vec())
}

fn fail_response_signature(_: &[u8]) -> Result<Vec<u8>, MessageSignatureError> {
    Err(MessageSignatureError::Signer("response failed".to_owned()))
}

#[derive(Clone, Default)]
struct RejectingConnector {
    attempts: Arc<AtomicUsize>,
}

impl RejectingConnector {
    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

impl ConnectorSend for RejectingConnector {
    type Stream = <TcpConnector as ConnectorSend>::Stream;

    fn connect(&self, _addr: SocketAddr) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        async { Err(io::Error::other("pre-I/O validation reached the connector")) }
    }

    fn connect_bound(
        &self,
        _addr: SocketAddr,
        _local: IpAddr,
    ) -> impl Future<Output = io::Result<Self::Stream>> + Send {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        async { Err(io::Error::other("pre-I/O validation reached the connector")) }
    }
}

fn rejecting_client() -> (
    HttpEngineSend<TokioRuntime, RejectingConnector>,
    RejectingConnector,
) {
    let connector = RejectingConnector::default();
    let client = HttpEngineSend::<TokioRuntime, RejectingConnector>::builder_with_connector(
        connector.clone(),
    )
    .build()
    .unwrap();
    (client, connector)
}

#[tokio::test]
async fn forward_response_signature_covers_response_hook_and_strips_hop_by_hop() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(200)
                            .header("connection", "x-upstream-hop")
                            .header("x-upstream-hop", "remove-me")
                            .body(Full::new(Bytes::from("ok")))
                            .unwrap(),
                    )
                }),
            )
            .await
            .unwrap();
    });

    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(std::str::from_utf8(base).unwrap().to_owned());
        Ok(b"resp".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::header(HeaderName::from_static(
            "x-gateway",
        )));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_response(|resp| {
            resp.headers_mut().insert(
                "x-gateway",
                http::header::HeaderValue::from_static("aioduct"),
            );
            resp.headers_mut().insert(
                http::header::CONNECTION,
                http::header::HeaderValue::from_static("x-hook-hop"),
            );
            resp.headers_mut().insert(
                "x-hook-hop",
                http::header::HeaderValue::from_static("remove-me-too"),
            );
        })
        .response_message_signature(config, signer)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.headers().get("signature").unwrap(), "sig1=:cmVzcA==:");
    assert!(!resp.headers().contains_key("connection"));
    assert!(!resp.headers().contains_key("x-upstream-hop"));
    assert!(!resp.headers().contains_key("x-hook-hop"));

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].contains(r#""@status": 200"#));
    assert!(bases[0].contains(r#""x-gateway": aioduct"#));
    assert!(!bases[0].contains("x-upstream-hop"));
    assert!(!bases[0].contains("x-hook-hop"));
}

#[tokio::test]
async fn forward_response_content_digest_is_signed_and_preserves_body() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("signed body"))))
                }),
            )
            .await
            .unwrap();
    });

    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(std::str::from_utf8(base).unwrap().to_owned());
        Ok(b"digest".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::header(HeaderName::from_static(
            CONTENT_DIGEST,
        )));

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/digest")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
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

    let expected_digest = sha256_content_digest_value(b"signed body").unwrap();
    assert_eq!(
        resp.headers().get(CONTENT_DIGEST).unwrap(),
        &expected_digest
    );
    assert_eq!(resp.headers().get("signature").unwrap(), "sig1=:ZGlnZXN0:");
    assert_eq!(resp.text().await.unwrap(), "signed body");

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].contains(r#""@status": 200"#));
    assert!(bases[0].contains(&format!(
        r#""content-digest": {}"#,
        expected_digest.to_str().unwrap()
    )));
}

#[tokio::test]
async fn forward_response_content_digest_rejects_body_over_limit() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("too large"))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/too-large")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_content_digest(4)
        .send()
        .await;

    match result.unwrap_err() {
        Error::Unsupported(message) => assert!(message.contains("buffer limit")),
        other => panic!("expected unsupported error, got {other:?}"),
    }
}

#[tokio::test]
async fn forward_response_content_digest_rejects_connect_before_upstream() {
    let (client, connector) = rejecting_client();
    let incoming_req = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri("example.com:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream("http://127.0.0.1:9".parse::<http::Uri>().unwrap())
        .response_content_digest(1024)
        .send()
        .await;

    match result.unwrap_err() {
        Error::Unsupported(message) => assert!(message.contains("CONNECT")),
        other => panic!("expected unsupported error, got {other:?}"),
    }
    assert_eq!(connector.attempts(), 0);
}

#[tokio::test]
async fn forward_response_content_digest_preserves_existing_field() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header(CONTENT_DIGEST, "sha-256=:YWJj:")
                            .body(Full::new(Bytes::from("existing digest body")))
                            .unwrap(),
                    )
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/existing")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_content_digest(0)
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.headers().get(CONTENT_DIGEST).unwrap(),
        "sha-256=:YWJj:"
    );
    assert_eq!(resp.text().await.unwrap(), "existing digest body");
}

#[tokio::test]
async fn forward_response_content_digest_skips_head_response() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    assert_eq!(req.method(), http::Method::HEAD);
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header(http::header::CONTENT_LENGTH, "11")
                            .body(Full::new(Bytes::from("hello world")))
                            .unwrap(),
                    )
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method(http::Method::HEAD)
        .uri("/head")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_content_digest(0)
        .send()
        .await
        .unwrap();

    assert!(!resp.headers().contains_key(CONTENT_DIGEST));
    assert_eq!(resp.content_length(), Some(11));
}

#[tokio::test]
async fn forward_response_content_digest_skips_not_modified_response() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(http::StatusCode::NOT_MODIFIED)
                            .body(Full::new(Bytes::new()))
                            .unwrap(),
                    )
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method(http::Method::GET)
        .uri("/cached")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
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
}

#[tokio::test]
async fn forward_response_signature_related_request_uses_inbound_request() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                        req.uri().to_string(),
                    ))))
                }),
            )
            .await
            .unwrap();
    });

    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(std::str::from_utf8(base).unwrap().to_owned());
        Ok(b"related".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::target_uri().related_request())
        .component(MessageSignatureComponent::authority().related_request())
        .component(MessageSignatureComponent::request_target().related_request())
        .component(MessageSignatureComponent::header(http::header::HOST).related_request());

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/public/items?x=1")
        .header("host", "downstream.example")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}/api", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .downstream_target_uri("https://downstream.example/public/items?x=1")
        .response_message_signature(config, signer)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "/api/public/items?x=1");

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(r#""@target-uri";req: https://downstream.example/public/items?x=1"#),
        "{}",
        bases[0]
    );
    assert!(bases[0].contains(r#""@authority";req: downstream.example"#));
    assert!(bases[0].contains(r#""@request-target";req: /public/items?x=1"#));
    assert!(bases[0].contains(r#""host";req: downstream.example"#));
    assert!(!bases[0].contains("127.0.0.1"));
    assert!(!bases[0].contains("/api/public"));
}

#[tokio::test]
async fn forward_response_signature_uses_semantic_h2_options_target() {
    let (upstream_addr, _) = aioduct_test_server::h1::h1_server_with(|_request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
    })
    .await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(std::str::from_utf8(base).unwrap().to_owned());
        Ok(b"related-options".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::target_uri().related_request())
        .component(MessageSignatureComponent::request_target().related_request());
    let request_uri = http::Uri::builder()
        .scheme("https")
        .authority("downstream.example")
        .path_and_query("*")
        .build()
        .unwrap();
    let incoming_request = Request::builder()
        .method(http::Method::OPTIONS)
        .uri(request_uri)
        .version(http::Version::HTTP_2)
        .header(http::header::HOST, "downstream.example")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(incoming_request))
        .upstream(
            format!("http://{upstream_addr}/proxy/base")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_message_signature(config, signer)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "ok");
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(r#""@target-uri";req: https://downstream.example"#),
        "{}",
        bases[0]
    );
    assert!(
        bases[0].contains(r#""@request-target";req: *"#),
        "{}",
        bases[0]
    );
    assert!(!bases[0].contains("/proxy/base"), "{}", bases[0]);
}

#[tokio::test]
async fn forward_response_signature_accepts_absolute_inbound_target_uri() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
                }),
            )
            .await
            .unwrap();
    });

    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(std::str::from_utf8(base).unwrap().to_owned());
        Ok(b"absolute".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::target_uri().related_request())
        .component(MessageSignatureComponent::scheme().related_request())
        .component(MessageSignatureComponent::authority().related_request());

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("https://downstream.example/full?x=1")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_message_signature(config, signer)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::OK);
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].contains(r#""@target-uri";req: https://downstream.example/full?x=1"#));
    assert!(bases[0].contains(r#""@scheme";req: https"#));
    assert!(bases[0].contains(r#""@authority";req: downstream.example"#));
}

#[tokio::test]
async fn forward_response_signature_requires_downstream_uri_before_upstream() {
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::target_uri().related_request());
    let (client, connector) = rejecting_client();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/origin-form")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream("http://127.0.0.1:9".parse::<http::Uri>().unwrap())
        .response_message_signature(config, unused_signature)
        .send()
        .await;

    match result.unwrap_err() {
        Error::Unsupported(message) => assert!(message.contains("downstream_target_uri")),
        other => panic!("expected unsupported error, got {other:?}"),
    }
    assert_eq!(connector.attempts(), 0);
}

#[tokio::test]
async fn forward_response_signature_rejects_trailers_before_upstream() {
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::header(HeaderName::from_static("expires")).trailer());
    let (client, connector) = rejecting_client();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/trailers")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream("http://127.0.0.1:9".parse::<http::Uri>().unwrap())
        .response_message_signature(config, unused_signature)
        .send()
        .await;

    match result.unwrap_err() {
        Error::Unsupported(message) => assert!(message.contains("trailer")),
        other => panic!("expected unsupported error, got {other:?}"),
    }
    assert_eq!(connector.attempts(), 0);
}

#[tokio::test]
async fn forward_response_signature_replaces_owned_label_and_preserves_others() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(200)
                            .header("signature-input", r#"old=("@status"), sig1=("x-stale")"#)
                            .header("signature", "old=:b2xk:, sig1=:c3RhbGU=:")
                            .body(Full::new(Bytes::from("ok")))
                            .unwrap(),
                    )
                }),
            )
            .await
            .unwrap();
    });

    let bases = Arc::new(Mutex::new(Vec::new()));
    let signer_bases = bases.clone();
    let signer = move |base: &[u8]| -> Result<Vec<u8>, MessageSignatureError> {
        signer_bases
            .lock()
            .unwrap()
            .push(std::str::from_utf8(base).unwrap().to_owned());
        Ok(b"new".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::header(HeaderName::from_static(
            "signature-input",
        )))
        .component(MessageSignatureComponent::header(HeaderName::from_static(
            "signature",
        )));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/labels")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_message_signature(config, signer)
        .send()
        .await
        .unwrap();

    let signature_input = resp
        .headers()
        .get("signature-input")
        .unwrap()
        .to_str()
        .unwrap();
    let signature = resp.headers().get("signature").unwrap().to_str().unwrap();
    assert!(signature_input.contains(r#"old=("@status")"#));
    assert!(signature_input.contains("sig1="));
    assert!(signature.contains("old=:b2xk:"));
    assert!(signature.contains("sig1=:bmV3:"));
    assert!(!signature.contains("c3RhbGU"));

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].contains(r#""signature-input": old=("@status")"#));
    assert!(bases[0].contains(r#""signature": old=:b2xk:"#));
    assert!(!bases[0].contains("x-stale"));
    assert!(!bases[0].contains("c3RhbGU"));
}

#[tokio::test]
async fn forward_response_signature_rechecks_request_after_on_request() {
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status());
    let (client, connector) = rejecting_client();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/connect-late")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream("http://127.0.0.1:9".parse::<http::Uri>().unwrap())
        .on_request(|parts| {
            parts.method = http::Method::CONNECT;
        })
        .response_message_signature(config, unused_signature)
        .send()
        .await;

    match result.unwrap_err() {
        Error::Unsupported(message) => assert!(message.contains("CONNECT")),
        other => panic!("expected unsupported error, got {other:?}"),
    }
    assert_eq!(connector.attempts(), 0);
}

#[tokio::test]
async fn forward_response_async_signing_is_included_in_timeout() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        server_attempts.fetch_add(1, Ordering::SeqCst);
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
                }),
            )
            .await
            .unwrap();
    });

    let signer = |_base: MessageSignatureBase| async move {
        std::future::pending::<()>().await;
        Ok::<_, MessageSignatureError>(b"late".to_vec())
    };
    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/timeout")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_message_signature_async(config, signer)
        .timeout(Duration::from_millis(20))
        .send()
        .await;

    assert!(matches!(result.unwrap_err(), Error::Timeout));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn forward_response_signing_failure_returns_error() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("unsigned"))))
                }),
            )
            .await
            .unwrap();
    });

    let config = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/fail")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(crate::valid_forward_request(incoming_req))
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .response_message_signature(config, fail_response_signature)
        .send()
        .await;

    match result.unwrap_err() {
        Error::MessageSignature(MessageSignatureError::Signer(message)) => {
            assert_eq!(message, "response failed");
        }
        other => panic!("expected signer error, got {other:?}"),
    }
}
