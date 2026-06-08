use std::convert::Infallible;
use std::net::SocketAddr;

use aioduct::Protocol;
use aioduct::SmolClient;
use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

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

async fn start_h2_ws_upstream() -> SocketAddr {
    use hyper::server::conn::http2 as server_http2;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
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
                                            let msg = String::from_utf8_lossy(&buf[..n]);
                                            let reply = format!("h2-echo: {msg}");
                                            if AsyncWriteExt::write_all(&mut io, reply.as_bytes())
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

    addr
}

fn main() -> Result<(), aioduct::Error> {
    // Tokio runtime for test server infrastructure
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    smol::block_on(async {
        let upstream_addr = start_h2_ws_upstream().await;
        println!("H2 WebSocket upstream running on {upstream_addr}");

        // Client must use H2 prior knowledge since we're connecting plaintext H2
        let client = SmolClient::builder().build().unwrap();

        // Build an H2 extended CONNECT request (RFC 8441)
        let mut incoming_req = http::Request::builder()
            .method(http::Method::CONNECT)
            .uri(format!("http://127.0.0.1:{}/ws/chat", upstream_addr.port()))
            .body(Full::new(Bytes::new()))
            .unwrap();
        incoming_req
            .extensions_mut()
            .insert(Protocol::from_static("websocket"));

        println!("\nForwarding H2 extended CONNECT to upstream...");

        let resp = client
            .forward(incoming_req)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .h2c()
            .send()
            .await?;

        println!(
            "Response status: {} (200 = tunnel established)",
            resp.status()
        );

        if resp.status() == http::StatusCode::OK {
            println!("H2 extended CONNECT tunnel established!\n");

            let mut upstream_io = resp.upgrade().await?;

            for msg in ["hello-h2", "websocket", "tunnel"] {
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

            println!("\nH2 WebSocket proxy tunnel working!");
        } else {
            println!("CONNECT failed: {}", resp.text().await?);
        }

        Ok(())
    })
}
