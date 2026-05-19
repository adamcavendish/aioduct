use std::convert::Infallible;
use std::net::SocketAddr;

use aioduct::SmolClient;
use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn start_ws_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|mut req: Request<hyper::body::Incoming>| async move {
                            if req.headers().get("upgrade").map(|v| v.as_bytes())
                                == Some(b"websocket")
                            {
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
                                            let msg = String::from_utf8_lossy(&buf[..n]);
                                            let reply = format!("echo: {msg}");
                                            if AsyncWriteExt::write_all(&mut io, reply.as_bytes())
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
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
                                Ok(Response::new(Full::new(Bytes::from("not a ws request"))))
                            }
                        }),
                    )
                    .with_upgrades()
                    .await;
            });
        }
    });

    addr
}

fn main() -> Result<(), aioduct::Error> {
    // Tokio runtime for test server infrastructure
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    smol::block_on(async {
        let upstream_addr = start_ws_upstream().await;
        println!("WebSocket upstream running on {upstream_addr}");

        let client = SmolClient::new();

        // Build a WebSocket upgrade request (as a gateway would receive from a client)
        let incoming_req = http::Request::builder()
            .method("GET")
            .uri("/ws/chat")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("sec-websocket-version", "13")
            .body(Full::new(Bytes::new()))
            .unwrap();

        println!("\nForwarding WebSocket upgrade to upstream...");

        let resp = client
            .forward(incoming_req)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .send()
            .await?;

        println!(
            "Response status: {} (101 = upgrade succeeded)",
            resp.status()
        );

        if resp.status() == http::StatusCode::SWITCHING_PROTOCOLS {
            println!("Upgrade successful! Getting bidirectional tunnel...\n");

            let mut upstream_io = resp.upgrade().await?;

            // In a real proxy, you'd splice this with the downstream Upgraded:
            //   tokio::io::copy_bidirectional(&mut downstream_io, &mut upstream_io).await?;
            //
            // Here we simulate the downstream side directly:
            for msg in ["hello", "world", "bye"] {
                AsyncWriteExt::write_all(&mut upstream_io, msg.as_bytes())
                    .await
                    .unwrap();
                let mut buf = vec![0u8; 1024];
                let n = AsyncReadExt::read(&mut upstream_io, &mut buf)
                    .await
                    .unwrap();
                println!(
                    "  sent: {msg:?} → received: {:?}",
                    &String::from_utf8_lossy(&buf[..n])
                );
            }

            println!("\nWebSocket proxy tunnel working!");
        } else {
            println!("Upgrade failed: {}", resp.text().await?);
        }

        Ok(())
    })
}
