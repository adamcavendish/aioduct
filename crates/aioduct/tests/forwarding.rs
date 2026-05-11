#![cfg(feature = "tokio")]

mod common;
use common::*;

// =============================================================================
// Request Forwarding Tests
// =============================================================================

#[tokio::test]
async fn forward_basic_get_to_upstream() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let path = req.uri().path().to_owned();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "upstream:{}",
                        path
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/hello/world")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "upstream:/hello/world");
}

#[tokio::test]
async fn forward_strip_prefix() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let path = req.uri().path().to_owned();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/api/v1/users")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .strip_prefix("/api")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "/v1/users");
}

#[tokio::test]
async fn forward_preserves_query_string() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let pq = req.uri().path_and_query().unwrap().as_str().to_owned();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(pq))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/search?q=rust&page=2")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "/search?q=rust&page=2");
}

#[tokio::test]
async fn forward_strips_hop_by_hop_headers() {
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
                    let has_te = req.headers().contains_key("te");
                    let has_custom = req.headers().contains_key("x-custom");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "conn={},te={},custom={}",
                        has_connection, has_te, has_custom
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("Connection", "keep-alive")
        .header("TE", "trailers")
        .header("X-Custom", "preserved")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.text().await.unwrap(),
        "conn=false,te=false,custom=true"
    );
}

#[tokio::test]
async fn forward_adds_extra_headers() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let xff = req
                        .headers()
                        .get("x-forwarded-for")
                        .map(|v| v.to_str().unwrap().to_owned())
                        .unwrap_or_default();
                    let rid = req
                        .headers()
                        .get("x-request-id")
                        .map(|v| v.to_str().unwrap().to_owned())
                        .unwrap_or_default();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "xff={},rid={}",
                        xff, rid
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .header(
            http::header::HeaderName::from_static("x-forwarded-for"),
            http::header::HeaderValue::from_static("10.0.0.1"),
        )
        .header(
            http::header::HeaderName::from_static("x-request-id"),
            http::header::HeaderValue::from_static("req-123"),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "xff=10.0.0.1,rid=req-123");
}

#[tokio::test]
async fn forward_preserve_host() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let host = req
                        .headers()
                        .get("host")
                        .map(|v| v.to_str().unwrap().to_owned())
                        .unwrap_or_default();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("host", "original.example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .preserve_host()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "original.example.com");
}

#[tokio::test]
async fn forward_rewrites_host_by_default() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let host = req
                        .headers()
                        .get("host")
                        .map(|v| v.to_str().unwrap().to_owned())
                        .unwrap_or_default();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(host))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("host", "original.example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    let host = resp.text().await.unwrap();
    assert!(
        host.contains("127.0.0.1"),
        "host should be rewritten to upstream, got: {}",
        host
    );
}

#[tokio::test]
async fn forward_post_with_body() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    use http_body_util::BodyExt;
                    let method = req.method().to_string();
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    let text = String::from_utf8(body.to_vec()).unwrap();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "{}:{}",
                        method, text
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("POST")
        .uri("/submit")
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from("hello body")))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "POST:hello body");
}

#[tokio::test]
async fn forward_on_request_hook() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let injected = req
                        .headers()
                        .get("x-injected")
                        .map(|v| v.to_str().unwrap().to_owned())
                        .unwrap_or_default();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(injected))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_request(|parts| {
            parts.headers.insert(
                http::header::HeaderName::from_static("x-injected"),
                http::header::HeaderValue::from_static("via-hook"),
            );
        })
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "via-hook");
}

#[tokio::test]
async fn forward_on_response_hook() {
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

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .on_response(|resp| {
            resp.headers_mut().insert(
                http::header::HeaderName::from_static("x-gateway"),
                http::header::HeaderValue::from_static("aioduct"),
            );
        })
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.headers().get("x-gateway").unwrap().to_str().unwrap(),
        "aioduct"
    );
}

#[tokio::test]
async fn forward_timeout() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("late"))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/slow")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let result = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .timeout(Duration::from_millis(50))
        .send()
        .await;

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), aioduct::Error::Timeout));
}

#[tokio::test]
async fn forward_remove_header() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let has_auth = req.headers().contains_key("authorization");
                    let has_custom = req.headers().contains_key("x-keep");
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
                        "auth={},keep={}",
                        has_auth, has_custom
                    )))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("authorization", "Bearer secret")
        .header("x-keep", "yes")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .remove_header(http::header::AUTHORIZATION)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "auth=false,keep=true");
}

#[tokio::test]
async fn forward_upstream_base_path() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let path = req.uri().path().to_owned();
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(path))))
                }),
            )
            .await
            .unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/users/123")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}/v2", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "/v2/users/123");
}

#[tokio::test]
async fn forward_h1_upgrade_websocket() {
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
                                let mut upgraded = aioduct::Upgraded::from(upgraded);
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

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
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
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
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

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/ws")
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
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

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let incoming_req = http::Request::builder()
        .method("GET")
        .uri("/test")
        .header("connection", "keep-alive")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = client
        .forward(incoming_req)
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

#[tokio::test]
async fn forward_h2_extended_connect() {
    use hyper::server::conn::http2 as server_http2;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
                        service_fn(|mut req: Request<hyper::body::Incoming>| async move {
                            if req.method() == http::Method::CONNECT {
                                tokio::spawn(async move {
                                    if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
                                        let mut io = aioduct::Upgraded::from(upgraded);
                                        let mut buf = vec![0u8; 1024];
                                        loop {
                                            let n =
                                                match AsyncReadExt::read(&mut io, &mut buf).await {
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
                        }),
                    )
                    .await;
            });
        }
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .build();

    let mut incoming_req = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri(format!("http://127.0.0.1:{}/ws/chat", upstream_addr.port()))
        .body(Full::new(Bytes::new()))
        .unwrap();
    incoming_req
        .extensions_mut()
        .insert(aioduct::Protocol::from_static("websocket"));

    let resp = client
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
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
}
