use std::convert::Infallible;
use std::net::SocketAddr;

use aioduct::{Multipart, Part, TokioClient};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

const FILE_BYTES: &[u8] = b"%PDF-1.4\nforwarded multipart upload\n%%EOF\n";

async fn start_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(|request: Request<hyper::body::Incoming>| async move {
            let content_type = request
                .headers()
                .get(http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let body = request.into_body().collect().await.unwrap().to_bytes();
            let received_file = body
                .windows(FILE_BYTES.len())
                .any(|window| window == FILE_BYTES);
            let summary = format!(
                "content-type={content_type}\nbytes={}\nfile-present={received_file}\n",
                body.len()
            );
            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(summary))))
        });
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        http1::Builder::new()
            .serve_connection(io, service)
            .await
            .unwrap();
    });
    address
}

async fn start_broker(upstream: SocketAddr) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = TokioClient::new();
    let upstream: http::Uri = format!("http://{upstream}").parse().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
            let client = client.clone();
            let upstream = upstream.clone();
            async move {
                let response = client.forward(request).upstream(upstream).send().await;
                let response = match response {
                    Ok(response) => {
                        let status = response.status();
                        match response.bytes().await {
                            Ok(body) => Response::builder()
                                .status(status)
                                .body(Full::new(body))
                                .unwrap(),
                            Err(error) => bad_gateway(error),
                        }
                    }
                    Err(error) => bad_gateway(error),
                };
                Ok::<_, Infallible>(response)
            }
        });
        let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
        http1::Builder::new()
            .serve_connection(io, service)
            .await
            .unwrap();
    });
    address
}

fn bad_gateway(error: aioduct::Error) -> Response<Full<Bytes>> {
    Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from(error.to_string())))
        .unwrap()
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let upstream = start_upstream().await;
    let broker = start_broker(upstream).await;
    let multipart = Multipart::new().text("model", "ocr-model").part(
        Part::bytes("file", FILE_BYTES.to_vec())
            .file_name("ocr-page.pdf")
            .mime_str("application/pdf"),
    );

    let broker_url = format!("http://{broker}/api/v2/ocr/jobs");
    let response = TokioClient::new()
        .post(&broker_url)?
        .multipart(multipart)
        .send()
        .await?;

    println!("status={}", response.status());
    println!("{}", response.text().await?);
    Ok(())
}
