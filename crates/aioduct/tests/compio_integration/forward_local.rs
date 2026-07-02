use super::*;

// ── Forward: on_request hook ──────────────────────────────────────────

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
            .forward_local(incoming)
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
            .forward_local(incoming)
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

// ── Forward: on_response hook ─────────────────────────────────────────

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
            .forward_local(incoming)
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
            .forward_local(incoming)
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
            .forward_local(incoming)
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
            .forward_local(incoming)
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
            .forward_local(incoming)
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
            .forward_local(incoming)
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
            .forward_local(incoming)
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

        let result = client.forward_local(incoming).send().await;
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
            .forward_local(incoming)
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
            .forward_local(incoming)
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
            .forward_local(incoming)
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
