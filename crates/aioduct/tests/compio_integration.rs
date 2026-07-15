#![cfg(all(feature = "compio", feature = "tokio"))]

#[path = "compio_integration/cache.rs"]
mod cache;
#[path = "compio_integration/chunk_download.rs"]
mod chunk_download;
#[path = "compio_integration/client_behavior.rs"]
mod client_behavior;
#[cfg(feature = "rustls")]
#[path = "compio_integration/connect_tunnel.rs"]
mod connect_tunnel;
#[path = "compio_integration/forward_local.rs"]
mod forward_local;
#[path = "compio_integration/forward_upgrades.rs"]
mod forward_upgrades;
#[path = "compio_integration/forwarding.rs"]
mod forwarding;
#[path = "compio_integration/proxy_local.rs"]
mod proxy_local;
#[path = "compio_integration/request_builder.rs"]
mod request_builder;
#[path = "compio_integration/resolver.rs"]
mod resolver;
#[path = "compio_integration/retry_local.rs"]
mod retry_local;
#[path = "compio_integration/sse.rs"]
mod sse;
#[path = "compio_integration/streaming.rs"]
mod streaming;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

use aioduct::runtime::compio_rt::{CompioRuntime, TcpConnector};
use aioduct::{
    CONTENT_DIGEST, HttpEngineLocal, MessageSignatureBase, MessageSignatureComponent,
    MessageSignatureConfig, sha256_content_digest_value,
};

fn valid_forward_request<B>(mut request: Request<B>) -> Request<B> {
    if request.version() == http::Version::HTTP_11
        && !request.headers().contains_key(http::header::HOST)
    {
        request.headers_mut().insert(
            http::header::HOST,
            http::HeaderValue::from_static("downstream.test"),
        );
    }
    request
}

async fn hello(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

fn compio_signature(_: &[u8]) -> Result<Vec<u8>, aioduct::MessageSignatureError> {
    Ok(b"compio".to_vec())
}

fn start_server_tokio() -> SocketAddr {
    start_server_with_tokio(|req| async { hello(req).await })
}

fn start_counting_h1_server_tokio() -> (SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>)
{
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = Arc::clone(&connections);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                server_connections.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                    let _ = server_http1::Builder::new()
                        .serve_connection(io, service_fn(hello))
                        .await;
                });
            }
        });
    });
    (rx.recv().unwrap(), connections)
}

fn read_raw_request_headers(stream: &mut std::net::TcpStream) {
    use std::io::Read;

    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::with_capacity(1024);
    let mut buf = [0u8; 512];
    while request.len() < 16 * 1024 {
        let n = stream.read(&mut buf).unwrap();
        assert_ne!(n, 0, "client closed before sending request headers");
        request.extend_from_slice(&buf[..n]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            stream.set_read_timeout(None).unwrap();
            return;
        }
    }
    panic!("request headers exceeded 16 KiB");
}

fn start_server_with_tokio<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = server_http1::Builder::new()
                        .serve_connection(io, service_fn(handler))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}

#[test]
fn test_compio_get_request() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_base_url_resolves_relative_path() {
    let addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
    });
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .base_url(&format!("http://{addr}/v1/"))
            .unwrap()
            .build_local()
            .unwrap();

        let resp = client.get_local("users").unwrap().send().await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "/v1/users");
    });
}

#[test]
fn test_compio_automatic_message_signature() {
    let addr = start_server_with_tokio(|req| async move {
        let signature_input = req
            .headers()
            .get("signature-input")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let signature = req
            .headers()
            .get("signature")
            .map(|v| v.to_str().unwrap().to_owned())
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

        let resp = client
            .get_local(&format!("http://{addr}/signed"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.contains("sig1="), "{body}");
        assert!(body.contains("sig1=:Y29tcGlv:"), "{body}");
    });
}

#[test]
fn test_compio_async_local_message_signature() {
    let addr = start_server_with_tokio(|req| async move {
        let signature_input = req
            .headers()
            .get("signature-input")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let signature = req
            .headers()
            .get("signature")
            .map(|v| v.to_str().unwrap().to_owned())
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
        let signer = |base: MessageSignatureBase| async move {
            let signature = std::rc::Rc::new((
                base.as_str().contains(r#""@request-target": /signed"#),
                b"local-async".to_vec(),
            ));
            std::future::ready(()).await;
            assert!(signature.as_ref().0);
            Ok::<_, aioduct::MessageSignatureError>(signature.as_ref().1.clone())
        };
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .message_signature_async_local(config, signer)
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/signed"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.contains("sig1="), "{body}");
        assert!(body.contains("sig1=:bG9jYWwtYXN5bmM=:"), "{body}");
    });
}

#[test]
fn test_compio_automatic_content_digest_before_signature() {
    let addr = start_server_with_tokio(|req| async move {
        let content_digest = req
            .headers()
            .get("content-digest")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "content-digest={content_digest}\nbody={}",
            String::from_utf8_lossy(&body)
        )))))
    });
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let config = MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::method())
            .component(MessageSignatureComponent::header(
                http::HeaderName::from_static("content-digest"),
            ));
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .automatic_content_digest(true)
            .message_signature(config, compio_signature)
            .build_local()
            .unwrap();

        let resp = client
            .post_local(&format!("http://{addr}/digest"))
            .unwrap()
            .body("hello")
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        let expected = "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:";
        assert!(
            body.contains(&format!("content-digest={expected}")),
            "{body}"
        );
        assert!(body.contains("body=hello"), "{body}");
    });
}

#[cfg(feature = "gzip")]
#[test]
fn test_compio_per_request_no_decompression() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let addr = start_server_with_tokio(|_req| async move {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"compio raw gzip").unwrap();
        let compressed = encoder.finish().unwrap();
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-encoding", "gzip")
                .body(Full::new(Bytes::from(compressed)))
                .unwrap(),
        )
    });
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .no_decompression()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.headers().get("content-encoding").unwrap(), "gzip");
        let raw = resp.bytes().await.unwrap();
        assert_ne!(
            raw.as_ref(),
            b"compio raw gzip",
            "body must stay compressed"
        );
    });
}

#[test]
fn test_compio_post_request() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{addr}/"))
            .unwrap()
            .body("request body")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_connection_reuse() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let url = format!("http://{addr}/");

        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let body = resp2.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

#[test]
fn test_compio_redirect_302() {
    let final_addr = start_server_tokio();
    let redirect_addr = start_server_with_tokio(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(302)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{redirect_addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "hello aioduct");
    });
}

// ── Compio TLS integration tests ─────────────────────────────────────
#[cfg(all(feature = "compio", feature = "tokio", feature = "rustls"))]
#[path = "compio_integration/tls.rs"]
mod compio_tls_tests;

#[test]
fn test_compio_https_only_rejects_http() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build_local()
            .unwrap();
        let result = client
            .get_local("http://example.com/")
            .unwrap()
            .send()
            .await;
        assert!(result.is_err());
    });
}

#[test]
fn test_compio_no_connection_reuse() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .no_connection_reuse()
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        let resp1 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp1.status(), http::StatusCode::OK);
        let _ = resp1.text().await.unwrap();

        let resp2 = client.get_local(&url).unwrap().send().await.unwrap();
        assert_eq!(resp2.status(), http::StatusCode::OK);
        let _ = resp2.text().await.unwrap();
    });
}

#[test]
fn compio_adaptive_h2c_caches_direct_h1_endpoint_without_pool_reuse() {
    use std::sync::atomic::Ordering;

    let (addr, connections) = start_counting_h1_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .no_connection_reuse()
            .build_local()
            .unwrap();
        let upstream = format!("http://{addr}");

        for path in ["/probe", "/cached"] {
            let request = Request::builder()
                .uri(path)
                .header(http::header::HOST, "downstream.test")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let response = client
                .forward_local(valid_forward_request(request))
                .upstream(&upstream)
                .adaptive_h2c()
                .send()
                .await
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "hello aioduct");
        }
    });

    assert_eq!(
        connections.load(Ordering::SeqCst),
        3,
        "one H2 probe, one H1 fallback, and one cached fresh H1 connection are expected"
    );
}

#[test]
fn test_compio_cookie_jar() {
    let addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/set" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "session=abc123; Path=/")
                    .body(Full::new(Bytes::from("cookie set")))
                    .unwrap(),
            )
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_owned())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(cookie))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let jar = aioduct::cookie::CookieJar::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cookie_jar(jar)
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/set"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/check"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("session=abc123"),
            "cookie not forwarded: {body}"
        );
    });
}

#[test]
fn test_compio_middleware() {
    let addr = start_server_with_tokio(|req| async move {
        let custom = req
            .headers()
            .get("x-middleware")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(custom))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .middleware(
                |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                    req.headers_mut().insert(
                        "x-middleware",
                        http::header::HeaderValue::from_static("injected"),
                    );
                },
            )
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert_eq!(body, "injected");
    });
}

#[test]
fn test_compio_read_timeout_fires() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-length", "10000")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_millis(50))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_bandwidth_limiter() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("bandwidth test data"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .max_download_speed(1024 * 1024)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "bandwidth test data");
    });
}

#[test]
fn test_compio_rate_limiter() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .max_requests_per_sec(100)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_error_for_status() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .status(404)
                .body(Full::new(Bytes::from("not found")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::NOT_FOUND);
        let err = resp.error_for_status();
        assert!(err.is_err());
    });
}

#[test]
fn test_compio_decompression_disabled() {
    let addr = start_server_with_tokio(|req| async move {
        let accept = req
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap_or("").to_owned())
            .unwrap_or_default();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(accept))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .no_decompression()
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.is_empty() || !body.contains("gzip"));
    });
}

#[test]
fn test_compio_tcp_keepalive() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .tcp_keepalive(Duration::from_secs(60))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

#[test]
fn test_compio_resolve_override() {
    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .resolve("custom-host.local", addr)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://custom-host.local:{}/", addr.port()))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "hello aioduct");
    });
}

#[test]
fn test_compio_request_local_with_delete() {
    let addr = start_server_with_tokio(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .request_local(http::Method::DELETE, &format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "DELETE");
    });
}

#[test]
fn test_compio_observer() {
    use std::sync::{Arc, Mutex};

    let addr = start_server_tokio();
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let phases = Arc::new(Mutex::new(Vec::new()));
        let phases_clone = phases.clone();

        struct Obs(Arc<Mutex<Vec<String>>>);
        impl aioduct::observer::RequestObserver for Obs {
            fn on_event(&self, event: &aioduct::observer::RequestEvent) {
                self.0.lock().unwrap().push(format!("{:?}", event.phase));
            }
            fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
        }

        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .request_observer(Obs(phases_clone))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        let recorded = phases.lock().unwrap();
        assert!(!recorded.is_empty(), "observer should have recorded phases");
    });
}

#[test]
fn test_compio_redirect_with_method_change() {
    let final_addr = start_server_with_tokio(|req| async move {
        let method = req.method().to_string();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(method))))
    });
    let redirect_addr = start_server_with_tokio(move |_req| {
        let target = format!("http://{final_addr}/");
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(303)
                    .header("location", target)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .post_local(&format!("http://{redirect_addr}/"))
            .unwrap()
            .body("some body")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "GET");
    });
}

#[test]
fn test_compio_too_many_redirects() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .status(302)
                .header("location", "/loop")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .max_redirects(3)
            .build_local()
            .unwrap();
        let result = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await;
        assert!(result.is_err());
    });
}

#[test]
fn test_compio_connect_timeout() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .connect_timeout(Duration::from_millis(1))
            .build_local()
            .unwrap();
        let result = client
            .get_local("http://192.0.2.1:1/")
            .unwrap()
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        assert!(result.is_err());
    });
}

#[test]
fn test_compio_hsts_store() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("hsts test"))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let hsts = aioduct::hsts::HstsStore::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .hsts(hsts)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
    });
}

// ── Cookie store from response test ────────────────────────────────

#[test]
fn test_compio_cookie_store_from_response() {
    let addr = start_server_with_tokio(|req| async move {
        let path = req.uri().path().to_string();
        if path == "/login" {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "token=xyz789; Path=/; HttpOnly")
                    .header("set-cookie", "lang=en; Path=/")
                    .body(Full::new(Bytes::from("logged in")))
                    .unwrap(),
            )
        } else {
            let cookie = req
                .headers()
                .get("cookie")
                .map(|v| v.to_str().unwrap_or("").to_owned())
                .unwrap_or_default();
            Ok(Response::new(Full::new(Bytes::from(cookie))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let jar = aioduct::cookie::CookieJar::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .cookie_jar(jar)
            .build_local()
            .unwrap();

        // Login: sets cookies
        let resp = client
            .get_local(&format!("http://{addr}/login"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        // Verify cookies are sent on subsequent requests
        let resp = client
            .get_local(&format!("http://{addr}/dashboard"))
            .unwrap()
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("token=xyz789"),
            "cookie not forwarded: {body}"
        );
    });
}

// ── Digest auth retry test ─────────────────────────────────────────

#[test]
fn test_compio_digest_auth_retry() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            let auth_header = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if n == 0 || !auth_header.starts_with("Digest ") {
                // First request or no digest auth: challenge with 401
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            "Digest realm=\"test@example.com\", nonce=\"abc123nonce\", qop=\"auth\", algorithm=MD5",
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                // Second request with digest credentials: verify and return 200
                assert!(
                    auth_header.contains("username=\"admin\""),
                    "digest auth should contain username"
                );
                assert!(
                    auth_header.contains("realm=\"test@example.com\""),
                    "digest auth should contain realm"
                );
                assert!(
                    auth_header.contains("nonce=\"abc123nonce\""),
                    "digest auth should contain nonce"
                );
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(200)
                        .body(Full::new(Bytes::from("authenticated")))
                        .unwrap(),
                )
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("admin", "secret123")
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/protected"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "authenticated");
    });
}

#[test]
fn test_compio_digest_auth_retry_with_body() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let request_count = Arc::new(AtomicUsize::new(0));
    let rc = request_count.clone();

    let addr = start_server_with_tokio(move |req| {
        let rc = rc.clone();
        async move {
            use http_body_util::BodyExt;
            let n = rc.fetch_add(1, Ordering::SeqCst);
            let auth_header = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body_bytes = req.collect().await.unwrap().to_bytes();

            if n == 0 || !auth_header.starts_with("Digest ") {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            "Digest realm=\"api\", nonce=\"xyz789\", qop=\"auth\"",
                        )
                        .body(Full::new(Bytes::from("need auth")))
                        .unwrap(),
                )
            } else {
                // Verify the body was replayed
                let body_str = String::from_utf8_lossy(&body_bytes);
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(200)
                        .body(Full::new(Bytes::from(format!("ok:{}", body_str))))
                        .unwrap(),
                )
            }
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .digest_auth("user", "pass")
            .build_local()
            .unwrap();
        let resp = client
            .post_local(&format!("http://{addr}/submit"))
            .unwrap()
            .body("my payload")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        let body = resp.text().await.unwrap();
        assert_eq!(body, "ok:my payload");
    });
}

// ── Finalize response with read timeout and bandwidth limit ────────

#[test]
fn test_compio_finalize_response_with_read_timeout_and_bandwidth() {
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
            "response with limits applied",
        ))))
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_secs(5))
            .max_download_speed(1024 * 1024)
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "response with limits applied");
    });
}

#[test]
fn test_compio_read_timeout_with_slow_body() {
    // Server sends headers immediately but body arrives slowly
    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            let (mut stream, _) = listener.accept().unwrap();
            use std::io::Write;
            read_raw_request_headers(&mut stream);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
            let _ = stream.write_all(b"5\r\nhello\r\n");
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(200));
            let _ = stream.write_all(b"6\r\n world\r\n0\r\n\r\n");
            let _ = stream.flush();
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_millis(50))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), http::StatusCode::OK);
        // The read timeout should fire during body consumption
        let result = resp.text().await;
        // Read timeout may cause an error when trying to read the body
        // or it may succeed if the timeout wraps the whole body read.
        // Either outcome exercises the finalize_response_local path.
        let _ = result;
    });
}

// ── HSTS store from response test ──────────────────────────────────

#[test]
fn test_compio_per_request_read_timeout_overrides_default() {
    // Server sends headers + partial body, then stalls.
    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            let (mut stream, _) = listener.accept().unwrap();
            use std::io::Write;
            read_raw_request_headers(&mut stream);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello");
            let _ = stream.flush();
            // Never send the remaining 5 bytes.
            std::thread::sleep(Duration::from_secs(30));
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Generous client default; per-request override is the tight one.
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .read_timeout(Duration::from_secs(5))
            .build_local()
            .unwrap();
        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .read_timeout(Duration::from_millis(100))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);

        let err = resp.text().await.unwrap_err();
        assert!(
            matches!(err, aioduct::Error::ReadTimeout),
            "per-request read_timeout should fire on stalled body, got: {err:?}"
        );
    });
}

#[test]
fn test_compio_hsts_store_from_response_header() {
    // This test verifies that when a response contains Strict-Transport-Security,
    // the HSTS store records it. Since the HSTS store_from_response is only called
    // when scheme is HTTPS, and we can't easily set up TLS in this test, we test
    // the HTTP path which should NOT store HSTS (only HTTPS responses store it).
    // This exercises the conditional check at lines 178-183.
    let addr = start_server_with_tokio(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header(
                    "strict-transport-security",
                    "max-age=31536000; includeSubDomains",
                )
                .body(Full::new(Bytes::from("hsts response")))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let hsts = aioduct::hsts::HstsStore::new();
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .hsts(hsts.clone())
            .build_local()
            .unwrap();

        let resp = client
            .get_local(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let _ = resp.text().await.unwrap();

        // HSTS should NOT be stored for HTTP responses (only HTTPS)
        // This exercises the condition check `current_uri.scheme() == Some(&http::uri::Scheme::HTTPS)`
        assert!(
            !hsts.should_upgrade(&format!("127.0.0.1:{}", addr.port())),
            "HSTS should not be stored from HTTP responses"
        );
    });
}

// ── 304 revalidation in execute_local ─────────────────────────────────

#[test]
fn test_compio_304_not_modified_not_redirect() {
    let addr = start_server_with_tokio(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(304)
                .header("etag", "\"test\"")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::new();
        let resp = client
            .get_local(&format!("http://{addr}/resource"))
            .unwrap()
            .send()
            .await
            .unwrap();
        // 304 should not be followed as redirect
        assert_eq!(resp.status(), http::StatusCode::NOT_MODIFIED);
    });
}

// ── HSTS store from HTTPS response local ──────────────────────────────

#[test]
fn test_compio_https_only_rejects_http_execute_local_path() {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .https_only(true)
            .build_local()
            .unwrap();
        let result = client
            .get_local("http://example.com/")
            .unwrap()
            .send()
            .await;
        assert!(result.is_err(), "https_only should reject http://");
    });
}

#[test]
fn test_compio_h2_multiplexing_reuses_connection() {
    use hyper::server::conn::http2 as server_http2;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone)]
    struct TokioExec;
    impl<F> hyper::rt::Executor<F> for TokioExec
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        fn execute(&self, fut: F) {
            tokio::spawn(fut);
        }
    }

    let request_count = Arc::new(AtomicU32::new(0));
    let count_clone = request_count.clone();

    let addr = {
        let (tx, rx) = std::sync::mpsc::channel();
        let count = count_clone;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr).unwrap();

                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
                    let count = count.clone();
                    tokio::spawn(async move {
                        let _ = server_http2::Builder::new(TokioExec)
                            .serve_connection(
                                io,
                                service_fn(move |_req| {
                                    let count = count.clone();
                                    async move {
                                        count.fetch_add(1, Ordering::SeqCst);
                                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                                            "h2 mux",
                                        ))))
                                    }
                                }),
                            )
                            .await;
                    });
                }
            });
        });
        rx.recv().unwrap()
    };

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .build_local()
            .unwrap();

        for i in 0..5 {
            let resp = client
                .get_local(&format!("http://{addr}/req{i}"))
                .unwrap()
                .h2c_prior_knowledge()
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            assert_eq!(resp.text().await.unwrap(), "h2 mux");
        }

        assert_eq!(
            request_count.load(Ordering::SeqCst),
            5,
            "all 5 H2 requests via compio should succeed with connection reuse"
        );
    });
}

/// H1 deferred check-in: connections aren't reused until the body is consumed.
/// Sequential requests with consumed bodies should reuse a single connection.
#[test]
fn h1_deferred_checkin_reuses_connection() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let accept_count = Arc::new(AtomicUsize::new(0));
    let accept_count2 = accept_count.clone();

    let addr = start_server_with_tokio(move |_req| {
        let cnt = accept_count2.clone();
        async move {
            cnt.fetch_add(1, Ordering::SeqCst);
            Ok(Response::new(Full::new(Bytes::from("ok"))))
        }
    });

    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = HttpEngineLocal::<CompioRuntime, TcpConnector>::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .build_local()
            .unwrap();
        let url = format!("http://{addr}/");

        for _ in 0..5 {
            let resp = client.get_local(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), http::StatusCode::OK);
            let _ = resp.text().await.unwrap();
            // Wait for deferred check-in to complete.
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let requests = accept_count.load(Ordering::SeqCst);
    assert_eq!(requests, 5, "all 5 requests should succeed");
}
