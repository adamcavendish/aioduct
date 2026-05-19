use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use aioduct::SmolClient;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

async fn start_sse_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|_req: Request<hyper::body::Incoming>| async move {
                            let body = "event: greeting\ndata: hello\n\nevent: update\ndata: world\n\nevent: done\ndata: bye\n\n";
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(200)
                                    .header("content-type", "text/event-stream")
                                    .header("cache-control", "no-cache")
                                    .body(Full::new(Bytes::from(body)))
                                    .unwrap(),
                            )
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
        let upstream_addr = start_sse_upstream().await;
        println!("SSE upstream running on {upstream_addr}");

        let client = SmolClient::new();

        let incoming_req = http::Request::builder()
            .method("GET")
            .uri("/events")
            .header("accept", "text/event-stream")
            .body(Full::new(Bytes::new()))
            .unwrap();

        println!("\nForwarding SSE request to upstream...\n");

        let resp = client
            .forward(incoming_req)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .timeout(Duration::from_secs(5))
            .send()
            .await?;

        println!("Response status: {}", resp.status());
        println!(
            "Content-Type: {:?}",
            resp.headers().get("content-type").unwrap()
        );

        let mut stream = resp.into_sse_stream();
        let mut count = 0;
        while let Some(event) = stream.next().await {
            match event {
                Ok(sse) => match sse {
                    aioduct::SseEvent::Message(m) => {
                        println!("SSE event: type={:?} data={:?}", m.event, m.data);
                        count += 1;
                    }
                    aioduct::SseEvent::Retry(ms) => {
                        println!("SSE retry: {ms}ms");
                    }
                },
                Err(e) => {
                    println!("Stream ended: {e}");
                    break;
                }
            }
        }
        println!("\nReceived {count} SSE events through the proxy");

        Ok(())
    })
}
