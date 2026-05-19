use std::convert::Infallible;
use std::net::SocketAddr;

use aioduct::SmolClient;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

async fn start_upstream() -> SocketAddr {
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
                        service_fn(|req: Request<hyper::body::Incoming>| async move {
                            let method = req.method().to_string();
                            let path = req.uri().path().to_owned();
                            let host = req
                                .headers()
                                .get("host")
                                .map(|v| v.to_str().unwrap_or("?").to_owned())
                                .unwrap_or_else(|| "missing".into());
                            let xff = req
                                .headers()
                                .get("x-forwarded-for")
                                .map(|v| v.to_str().unwrap_or("?").to_owned())
                                .unwrap_or_else(|| "none".into());

                            let body = format!(
                                "upstream received:\n  method={method}\n  path={path}\n  host={host}\n  x-forwarded-for={xff}"
                            );
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body))))
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
        let upstream_addr = start_upstream().await;
        println!("Upstream running on {upstream_addr}");

        let client = SmolClient::new();

        // Simulate an incoming request to /api/users from 10.0.0.42
        let incoming_req = http::Request::builder()
            .method("GET")
            .uri("/api/users?page=2")
            .header("host", "public-gateway.example.com")
            .body(Full::new(Bytes::new()))
            .unwrap();

        println!("\nForwarding: GET /api/users?page=2 → upstream with /api stripped\n");

        let resp = client
            .forward(incoming_req)
            .upstream(
                format!("http://127.0.0.1:{}", upstream_addr.port())
                    .parse::<http::Uri>()
                    .unwrap(),
            )
            .strip_prefix("/api")
            .header(
                http::header::HeaderName::from_static("x-forwarded-for"),
                http::header::HeaderValue::from_static("10.0.0.42"),
            )
            .send()
            .await?;

        println!("Response status: {}", resp.status());
        println!("Response body:\n{}", resp.text().await?);

        Ok(())
    })
}
