#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_upgrade_websocket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);

        let conn = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                io,
                hyper::service::service_fn(
                    |mut req: hyper::Request<hyper::body::Incoming>| async move {
                        if req.headers().get("upgrade").map(|v| v.as_bytes()) == Some(b"websocket")
                        {
                            tokio::spawn(async move {
                                if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
                                    let mut upgraded = aioduct::Upgraded::from(upgraded);
                                    let mut buf = vec![0u8; 64];
                                    let n =
                                        AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
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
                    },
                ),
            )
            .with_upgrades();

        conn.await.unwrap();
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .upgrade()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);

    let mut upgraded = resp.upgrade().await.unwrap();

    AsyncWriteExt::write_all(&mut upgraded, b"hello upgrade")
        .await
        .unwrap();

    let mut buf = vec![0u8; 64];
    let n = AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello upgrade");
}
#[tokio::test]
async fn test_upgrade_flush_and_shutdown() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        let _ = server_http1::Builder::new()
            .serve_connection(
                io,
                service_fn(|mut req: Request<hyper::body::Incoming>| async move {
                    if req.headers().get("upgrade").map(|v| v.as_bytes()) == Some(b"websocket") {
                        tokio::spawn(async move {
                            if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
                                let mut upgraded = aioduct::Upgraded::from(upgraded);
                                let mut buf = vec![0u8; 1024];
                                if let Ok(n) = AsyncReadExt::read(&mut upgraded, &mut buf).await {
                                    let _ =
                                        AsyncWriteExt::write_all(&mut upgraded, &buf[..n]).await;
                                    let _ = AsyncWriteExt::flush(&mut upgraded).await;
                                    let _ = AsyncWriteExt::shutdown(&mut upgraded).await;
                                }
                            }
                        });
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(101)
                                .header("upgrade", "websocket")
                                .header("connection", "upgrade")
                                .body(Full::new(Bytes::new()))
                                .unwrap(),
                        )
                    } else {
                        Ok(Response::new(Full::new(Bytes::from("not an upgrade"))))
                    }
                }),
            )
            .with_upgrades()
            .await;
    });

    let client = HttpEngine::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let resp = client
        .get(&format!("http://{addr}/ws"))
        .unwrap()
        .upgrade()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), http::StatusCode::SWITCHING_PROTOCOLS);
    let mut upgraded = resp.upgrade().await.unwrap();
    let dbg = format!("{upgraded:?}");
    assert!(dbg.contains("Upgraded"));
    AsyncWriteExt::write_all(&mut upgraded, b"ping")
        .await
        .unwrap();
    AsyncWriteExt::flush(&mut upgraded).await.unwrap();

    let mut buf = vec![0u8; 1024];
    let n = AsyncReadExt::read(&mut upgraded, &mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"ping");

    let _inner = upgraded.into_inner();
}
