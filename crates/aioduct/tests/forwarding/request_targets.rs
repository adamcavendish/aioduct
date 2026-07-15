use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct::{
    HttpEngineSend, MessageSignatureComponent, MessageSignatureConfig, MessageSignatureError,
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn connect_signature_config() -> MessageSignatureConfig {
    MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::authority())
        .component(MessageSignatureComponent::target_uri())
        .component(MessageSignatureComponent::request_target())
}

fn connect_recording_signer(
    bases: Arc<Mutex<Vec<String>>>,
) -> impl Fn(&[u8]) -> Result<Vec<u8>, MessageSignatureError> + Send + Sync + 'static {
    move |base| {
        bases
            .lock()
            .unwrap()
            .push(String::from_utf8(base.to_vec()).unwrap());
        Ok(b"signed".to_vec())
    }
}

fn assert_connect_signature_target(bases: &Arc<Mutex<Vec<String>>>) {
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(r#""@authority": target.example:443"#),
        "{}",
        bases[0]
    );
    assert!(
        bases[0].contains(r#""@target-uri": http://target.example:443"#),
        "{}",
        bases[0]
    );
    assert!(
        bases[0].contains(r#""@request-target": target.example:443"#),
        "{}",
        bases[0]
    );
    assert!(!bases[0].contains("/proxy/base"), "{}", bases[0]);
}

fn options_signature_config() -> MessageSignatureConfig {
    MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::target_uri())
        .component(MessageSignatureComponent::request_target())
}

fn assert_options_signature_target(bases: &Arc<Mutex<Vec<String>>>, expected_target_uri: &str) {
    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(&format!(r#""@target-uri": {expected_target_uri}"#)),
        "{}",
        bases[0]
    );
    assert!(bases[0].contains(r#""@request-target": *"#), "{}", bases[0]);
    assert!(!bases[0].contains("/proxy/base"), "{}", bases[0]);
}

fn h2_wire_target<B>(request: &Request<B>) -> String {
    // h2 reconstructs this URI directly from the received pseudo-header block,
    // so a missing URI authority proves that `:authority` was absent on wire.
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
    format!(
        "version={:?};authority={authority};host={host};path={}",
        request.version(),
        request.uri().path_and_query().unwrap().as_str()
    )
}

fn h2_asterisk_uri(scheme: &str, authority: &str) -> http::Uri {
    // This is how an authority-bearing, server-wide OPTIONS target is exposed
    // after HTTP/2 has mapped its empty absolute-form path to `:path = *`.
    http::Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query("*")
        .build()
        .unwrap()
}

#[tokio::test]
async fn forward_http1_connect_uses_preserved_wire_authority_and_signing_target() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 3\r\n\r\nbad")
            .await
            .unwrap();
    });

    let bases = Arc::new(Mutex::new(Vec::new()));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(
            connect_signature_config(),
            connect_recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .header(http::header::HOST, "target.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://{addr}/proxy/base")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    let request = String::from_utf8(request_rx.await.unwrap()).unwrap();
    assert!(
        request.starts_with("CONNECT target.example:443 HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("\r\nhost: target.example:443\r\n"),
        "{request}"
    );
    assert_connect_signature_target(&bases);
}

#[tokio::test]
async fn forward_http11_connect_rejects_host_without_target_port_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_11)
        .header(http::header::HOST, "target.example")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "invalid CONNECT Host reached upstream I/O"
    );
}

#[tokio::test]
async fn forward_http2_connect_uses_preserved_wire_authority_and_signing_target() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        assert_eq!(request.method(), http::Method::CONNECT);
        assert_eq!(request.uri().authority().unwrap(), "target.example:443");
        Ok::<_, Infallible>(
            Response::builder()
                .status(http::StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from_static(b"bad")))
                .unwrap(),
        )
    })
    .await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(
            connect_signature_config(),
            connect_recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("target.example:443")
        .version(http::Version::HTTP_2)
        .header(http::header::HOST, "target.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://{addr}/proxy/base")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    assert_connect_signature_target(&bases);
}

#[tokio::test]
async fn forward_http2_connect_rejects_mismatched_host_before_io() {
    for host in [
        "different.example:443",
        "target.example",
        "target.example:444",
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let request = Request::builder()
            .method(http::Method::CONNECT)
            .uri("target.example:443")
            .version(http::Version::HTTP_2)
            .header(http::header::HOST, host)
            .body(Full::new(Bytes::new()))
            .unwrap();

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
            "mismatched HTTP/2 CONNECT Host {host:?} reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn forward_http1_server_wide_options_preserves_wire_target() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{} {} {:?}",
            request.method(),
            request.uri(),
            request.version(),
        )))))
    })
    .await;
    for (target, expected_target, expect_hook_asterisk) in [
        (http::Uri::from_static("*"), "*", true),
        (
            http::Uri::from_static("http://downstream.test/"),
            "/",
            false,
        ),
        (
            http::Uri::from_static("http://downstream.test?x=1"),
            "/?x=1",
            false,
        ),
    ] {
        let request = Request::builder()
            .method(http::Method::OPTIONS)
            .uri(target)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .on_request(move |parts| {
                if expect_hook_asterisk {
                    assert_eq!(parts.uri, "*");
                }
            })
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            format!("OPTIONS {expected_target} HTTP/1.1")
        );
    }
}

#[tokio::test]
async fn forward_explicit_h2c_options_preserves_wire_target_provenance() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(h2_wire_target(
            &request,
        )))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let upstream_authority = addr.to_string();
    let pathless_absolute = h2_asterisk_uri("http", "downstream.test");
    for (target, version, expected_authority, expected_path) in [
        (
            http::Uri::from_static("*"),
            http::Version::HTTP_11,
            "missing".to_owned(),
            "*",
        ),
        (
            pathless_absolute,
            http::Version::HTTP_2,
            upstream_authority.clone(),
            "*",
        ),
        (
            http::Uri::from_static("http://downstream.test/"),
            http::Version::HTTP_11,
            upstream_authority.clone(),
            "/",
        ),
        (
            http::Uri::from_static("http://downstream.test?x=1"),
            http::Version::HTTP_11,
            upstream_authority.clone(),
            "/?x=1",
        ),
    ] {
        let request = Request::builder()
            .method(http::Method::OPTIONS)
            .uri(target)
            .version(version)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            format!(
                "version=HTTP/2.0;authority={expected_authority};host={upstream_authority};path={expected_path}"
            )
        );
    }
}

#[tokio::test]
async fn forward_explicit_h2c_hooks_rewrite_options_target_provenance() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(h2_wire_target(
            &request,
        )))))
    })
    .await;
    let upstream_authority = addr.to_string();
    let inbound_pathless = h2_asterisk_uri("http", "downstream.test");
    let hook_pathless = h2_asterisk_uri("http", &upstream_authority);

    for (inbound, hook, hook_sees_authority, expected_authority) in [
        (
            http::Uri::from_static("*"),
            hook_pathless,
            false,
            upstream_authority.clone(),
        ),
        (
            inbound_pathless,
            http::Uri::from_static("*"),
            true,
            "missing".to_owned(),
        ),
    ] {
        let request = Request::builder()
            .method(http::Method::OPTIONS)
            .uri(inbound)
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let hook = hook.clone();
        let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .h2c()
            .on_request(move |parts| {
                assert_eq!(parts.uri.authority().is_some(), hook_sees_authority);
                parts.uri = hook;
            })
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            format!(
                "version=HTTP/2.0;authority={expected_authority};host={upstream_authority};path=*"
            )
        );
    }
}

#[tokio::test]
async fn forward_h2c_options_signature_uses_asterisk_semantics() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        assert_eq!(request.method(), http::Method::OPTIONS);
        assert_eq!(request.uri().path(), "*");
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    })
    .await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(
            options_signature_config(),
            connect_recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::OPTIONS)
        .uri("*")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://{addr}/proxy/base")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_options_signature_target(&bases, &format!("http://{addr}"));
}

#[tokio::test]
async fn forward_h2c_signature_uses_semantic_origin_request_target() {
    let (addr, _) = aioduct_test_server::h2::h2_server_with(|request| async move {
        assert_eq!(
            request.uri().path_and_query().unwrap(),
            "/proxy/base/resource?x=1"
        );
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
    })
    .await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .message_signature(
            options_signature_config(),
            connect_recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/resource?x=1")
        .body(Full::new(Bytes::new()))
        .unwrap();

    client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("http://{addr}/proxy/base")
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
        .send()
        .await
        .unwrap();

    let bases = bases.lock().unwrap();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].contains(&format!(
            r#""@target-uri": http://{addr}/proxy/base/resource?x=1"#
        )),
        "{}",
        bases[0]
    );
    assert!(
        bases[0].contains(r#""@request-target": /proxy/base/resource?x=1"#),
        "{}",
        bases[0]
    );
    assert!(
        !bases[0].contains(&format!(
            r#""@request-target": http://{addr}/proxy/base/resource?x=1"#
        )),
        "{}",
        bases[0]
    );
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_negotiated_https_uses_h1_for_authority_omitted_options() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) =
        aioduct_test_server::tls::tls_server_with(&[b"h2", b"http/1.1"], |request| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(h2_wire_target(
                &request,
            )))))
        })
        .await;
    let connector =
        aioduct::tls::RustlsConnector::new(aioduct_test_server::tls::make_client_config(&cert));
    let bases = Arc::new(Mutex::new(Vec::new()));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(connector)
        .message_signature(
            options_signature_config(),
            connect_recording_signer(bases.clone()),
        )
        .build()
        .unwrap();

    let request = Request::builder()
        .method(http::Method::OPTIONS)
        .uri("*")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.text().await.unwrap(),
        format!(
            "version=HTTP/1.1;authority=missing;host=localhost:{};path=*",
            addr.port()
        )
    );
    assert_options_signature_target(&bases, &format!("https://localhost:{}", addr.port()));
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_negotiated_https_hooks_rewrite_options_target_provenance() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, _) =
        aioduct_test_server::tls::tls_server_with(&[b"h2", b"http/1.1"], |request| async move {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(h2_wire_target(
                &request,
            )))))
        })
        .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&cert),
        ))
        .build()
        .unwrap();
    let upstream_authority = format!("localhost:{}", addr.port());
    let upstream = format!("https://{upstream_authority}")
        .parse::<http::Uri>()
        .unwrap();
    let inbound_pathless = h2_asterisk_uri("https", "downstream.test");

    let request = Request::builder()
        .method(http::Method::OPTIONS)
        .uri(inbound_pathless.clone())
        .version(http::Version::HTTP_2)
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(upstream.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.text().await.unwrap(),
        format!("version=HTTP/2.0;authority={upstream_authority};host={upstream_authority};path=*")
    );

    let hook_pathless = h2_asterisk_uri("https", &upstream_authority);
    for (inbound, hook, hook_sees_authority, expected_version, expected_authority) in [
        (
            http::Uri::from_static("*"),
            hook_pathless,
            false,
            "HTTP/2.0",
            upstream_authority.clone(),
        ),
        (
            inbound_pathless,
            http::Uri::from_static("*"),
            true,
            "HTTP/1.1",
            "missing".to_owned(),
        ),
    ] {
        let request = Request::builder()
            .method(http::Method::OPTIONS)
            .uri(inbound)
            .version(http::Version::HTTP_2)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let hook = hook.clone();
        let response = client
            .forward(crate::valid_forward_request(request))
            .upstream(upstream.clone())
            .on_request(move |parts| {
                assert_eq!(parts.uri.authority().is_some(), hook_sees_authority);
                parts.uri = hook;
            })
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            format!(
                "version={expected_version};authority={expected_authority};host={upstream_authority};path=*"
            )
        );
    }
}

#[cfg(feature = "rustls")]
#[tokio::test]
async fn forward_exact_h2_rejects_authority_omitted_https_options_before_io() {
    aioduct_test_server::tls::install_crypto_provider();
    let (addr, cert, counter) =
        aioduct_test_server::tls::tls_server_with(&[b"h2", b"http/1.1"], |_request| async {
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
        })
        .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::new(
            aioduct_test_server::tls::make_client_config(&cert),
        ))
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::OPTIONS)
        .uri("*")
        .version(http::Version::HTTP_11)
        .header(http::header::HOST, "downstream.test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(request)
        .upstream(
            format!("https://localhost:{}", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_2)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("cannot be represented by the HTTP/2 transport")),
        "{error}"
    );
    assert_eq!(counter.connections(), 0);
    assert_eq!(counter.requests(), 0);
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_http3_options_targets_preserve_pseudo_headers() {
    let (addr, _, counter) = aioduct_test_server::h3::h3_server_with(|request, _body| {
        let scheme = request.uri().scheme_str().unwrap_or("missing");
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
        (
            http::StatusCode::OK,
            Bytes::from(format!(
                "method={};version={:?};scheme={scheme};authority={authority};host={host};path={}",
                request.method(),
                request.version(),
                request.uri().path_and_query().unwrap().as_str()
            )),
        )
    })
    .await;
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .build()
        .unwrap();
    let upstream_authority = format!("127.0.0.1:{}", addr.port());
    let asterisk = Request::builder()
        .method(http::Method::OPTIONS)
        .uri("*")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let error = client
        .forward(crate::valid_forward_request(asterisk))
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
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("authority-free OPTIONS *")),
        "{error:?}"
    );
    assert_eq!(counter.connections(), 0);
    assert_eq!(counter.requests(), 0);

    for (target, expected_target_authority, expected_path) in [
        (
            http::Uri::from_static("http://downstream.test/"),
            upstream_authority.clone(),
            "/",
        ),
        (
            http::Uri::from_static("http://downstream.test?x=1"),
            upstream_authority.clone(),
            "/?x=1",
        ),
    ] {
        let request = Request::builder()
            .method(http::Method::OPTIONS)
            .uri(target)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let response = client
            .forward(crate::valid_forward_request(request))
            .upstream(
                format!("https://127.0.0.1:{}", addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .on_request(|parts| parts.version = http::Version::HTTP_3)
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.text().await.unwrap(),
            format!(
                "method=OPTIONS;version=HTTP/3.0;scheme=https;authority={expected_target_authority};host={upstream_authority};path={expected_path}",
            )
        );
    }
}

#[cfg(all(feature = "rustls", feature = "http3"))]
#[tokio::test]
async fn forward_http3_authority_free_options_fails_before_signing() {
    let (addr, _, counter) = aioduct_test_server::h3::h3_server().await;
    let bases = Arc::new(Mutex::new(Vec::new()));
    let client: HttpEngineSend<TokioRuntime, TcpConnector> = HttpEngineSend::builder()
        .tls(aioduct::tls::RustlsConnector::danger_accept_invalid_certs())
        .http3(true)
        .unwrap()
        .message_signature(
            options_signature_config(),
            connect_recording_signer(bases.clone()),
        )
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::OPTIONS)
        .uri("*")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(
            format!("https://127.0.0.1:{}/proxy/base", addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| parts.version = http::Version::HTTP_3)
        .send()
        .await
        .unwrap_err();

    assert!(
        matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("authority-free OPTIONS *")),
        "{error:?}"
    );
    assert!(bases.lock().unwrap().is_empty());
    assert_eq!(counter.connections(), 0);
    assert_eq!(counter.requests(), 0);
}

#[tokio::test]
async fn absolute_targets_preserve_explicit_path_and_query() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{} {}",
            request.method(),
            request.uri()
        )))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();

    for (method, target, expected) in [
        (http::Method::GET, "http://downstream.test/", "GET /"),
        (
            http::Method::OPTIONS,
            "http://downstream.test/",
            "OPTIONS /",
        ),
        (
            http::Method::OPTIONS,
            "http://downstream.test?x=1",
            "OPTIONS /?x=1",
        ),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(target)
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = client
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), expected);
    }
}

#[tokio::test]
async fn ordinary_on_request_method_rewrite_remains_allowed() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
            request.method().as_str().to_owned(),
        ))))
    })
    .await;
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/method-rewrite")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_request(|parts| parts.method = http::Method::POST)
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "POST");
}

#[tokio::test]
async fn on_request_rejects_head_and_connect_semantic_class_rewrites_before_io() {
    for (downstream, target, upstream) in [
        (http::Method::GET, "/to-head", http::Method::HEAD),
        (http::Method::HEAD, "/from-head", http::Method::GET),
        (http::Method::GET, "/to-connect", http::Method::CONNECT),
        (
            http::Method::CONNECT,
            "target.example:443",
            http::Method::GET,
        ),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let request = Request::builder()
            .method(downstream.clone())
            .uri(target)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(crate::valid_forward_request(request))
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .on_request(move |parts| parts.method = upstream.clone())
            .send()
            .await
            .unwrap_err();

        assert!(
            matches!(error, aioduct::Error::Unsupported(ref message) if message.contains("HEAD or CONNECT")),
            "{downstream} rewrite returned {error}"
        );
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "{downstream} semantic rewrite reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn ordinary_connect_hook_can_retarget_with_authority_form() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 3\r\n\r\nbad")
            .await
            .unwrap();
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("original.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_request(|parts| parts.uri = "retarget.example:8443".parse().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    let request = String::from_utf8(request_rx.await.unwrap()).unwrap();
    assert!(
        request.starts_with("CONNECT retarget.example:8443 HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("\r\nhost: retarget.example:8443\r\n"),
        "{request}"
    );
}

#[tokio::test]
async fn ordinary_connect_hook_rejects_ambiguous_absolute_target_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("original.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_request(|parts| parts.uri = "http://retarget.example:8443/".parse().unwrap())
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidUrl(_)), "{error}");
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "invalid CONNECT target must be rejected before TCP I/O"
    );
}

#[tokio::test]
async fn ordinary_connect_hook_rejects_portless_authority_before_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let request = Request::builder()
        .method(http::Method::CONNECT)
        .uri("original.example:443")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(crate::valid_forward_request(request))
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .on_request(|parts| parts.uri = "retarget.example".parse().unwrap())
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidUrl(_)), "{error}");
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "hook-retargeted portless CONNECT must be rejected before TCP I/O"
    );
}

#[tokio::test]
async fn malformed_inbound_request_targets_are_rejected_before_upstream_io() {
    for (method, target) in [(http::Method::GET, "relative")] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let request = Request::builder()
            .method(method)
            .uri(target)
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "downstream.test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidUrl(_)), "{error}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "malformed target {target:?} reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn malformed_on_request_targets_are_rejected_before_upstream_io() {
    for target in ["relative"] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let hook_uri: http::Uri = target.parse().unwrap();
        let request = Request::builder()
            .method(http::Method::OPTIONS)
            .uri("/valid")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "downstream.test")
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .on_request(move |parts| parts.uri = hook_uri.clone())
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidUrl(_)), "{error}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "malformed hook target {target:?} reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn http11_missing_host_is_rejected_before_upstream_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/missing-host")
        .version(http::Version::HTTP_11)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "missing HTTP/1.1 Host must be rejected before TCP I/O"
    );
}

#[tokio::test]
async fn http11_present_empty_host_is_forwarded_with_upstream_authority() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
            request
                .headers()
                .get(http::header::HOST)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
        ))))
    })
    .await;
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/empty-host")
        .version(http::Version::HTTP_11)
        .header(http::header::HOST, http::HeaderValue::from_static(""))
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), addr.to_string());
}

#[tokio::test]
async fn http11_duplicate_or_invalid_host_is_rejected_before_upstream_io() {
    for hosts in [vec!["one.example", "two.example"], vec!["bad host"]] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut request = Request::builder()
            .method(http::Method::GET)
            .uri("/invalid-host")
            .version(http::Version::HTTP_11)
            .body(Full::new(Bytes::new()))
            .unwrap();
        for host in hosts {
            request.headers_mut().append(
                http::header::HOST,
                http::HeaderValue::try_from(host).unwrap(),
            );
        }

        let error = HttpEngineSend::<TokioRuntime, TcpConnector>::new()
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidHeader(_)), "{error}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "invalid HTTP/1.1 Host reached upstream I/O"
        );
    }
}

#[tokio::test]
async fn http10_without_host_remains_valid() {
    let (addr, _) = aioduct_test_server::h1::h1_server_with(|request| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "{:?} {}",
            request.version(),
            request.uri()
        )))))
    })
    .await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("/legacy")
        .version(http::Version::HTTP_10)
        .body(Full::new(Bytes::new()))
        .unwrap();

    let response = client
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap();

    assert_eq!(response.text().await.unwrap(), "HTTP/1.1 /legacy");
}

#[tokio::test]
async fn non_connect_authority_form_is_rejected_before_upstream_io() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let authority_uri = http::Uri::builder()
        .authority("target.example:80")
        .build()
        .unwrap();
    let request = Request::builder()
        .method(http::Method::GET)
        .uri(authority_uri)
        .header(http::header::HOST, "target.example:80")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let error = client
        .forward(request)
        .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
        .send()
        .await
        .unwrap_err();

    assert!(matches!(error, aioduct::Error::InvalidUrl(_)), "{error}");
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "invalid authority-form GET must be rejected before TCP I/O"
    );
}

#[tokio::test]
async fn ordinary_connect_requires_an_explicit_port_before_upstream_io() {
    for authority in ["target.example", "[::1]"] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
        let authority_uri = http::Uri::builder().authority(authority).build().unwrap();
        let request = Request::builder()
            .method(http::Method::CONNECT)
            .uri(authority_uri)
            .header(http::header::HOST, authority)
            .body(Full::new(Bytes::new()))
            .unwrap();

        let error = client
            .forward(request)
            .upstream(format!("http://{addr}").parse::<http::Uri>().unwrap())
            .send()
            .await
            .unwrap_err();

        assert!(matches!(error, aioduct::Error::InvalidUrl(_)), "{error}");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "portless CONNECT authority must be rejected before TCP I/O"
        );
    }
}
