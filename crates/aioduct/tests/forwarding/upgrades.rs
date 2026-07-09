use std::convert::Infallible;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;
use aioduct_test_server::TokioExec;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
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
async fn forward_upgrade_field_without_connection_upgrade_token_strips_connection() {
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
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.text().await.unwrap(), "conn=false,upgrade=true");
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
                                        let mut io = aioduct::UpgradedSend::from(upgraded);
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

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
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
        .forward(incoming_req)
        .upstream(
            format!("http://127.0.0.1:{}", upstream_addr.port())
                .parse::<http::Uri>()
                .unwrap(),
        )
        .h2c()
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
