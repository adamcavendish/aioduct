use std::convert::Infallible;
use std::net::SocketAddr;

use aioduct::{CompioClient, Multipart, Part};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};

const FILE_BYTES: &[u8] = b"%PDF-1.4\nforwarded multipart upload\n%%EOF\n";

async fn start_upstream() -> SocketAddr {
    let listener = compio_net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    compio_runtime::spawn(async move {
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
        let io = Box::pin(aioduct::runtime::compio_rt::CompioIo::new(
            compio_io::compat::AsyncStream::new(stream),
        ));
        http1::Builder::new()
            .serve_connection(io, service)
            .await
            .unwrap();
    })
    .detach();
    address
}

async fn start_broker(upstream: SocketAddr) -> SocketAddr {
    let listener = compio_net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = CompioClient::builder().build_local().unwrap();
    let upstream: http::Uri = format!("http://{upstream}").parse().unwrap();
    compio_runtime::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(move |request: Request<hyper::body::Incoming>| {
            let client = client.clone();
            let upstream = upstream.clone();
            async move {
                let response = client
                    .forward_local(request)
                    .upstream(upstream)
                    .send()
                    .await;
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
        let io = Box::pin(aioduct::runtime::compio_rt::CompioIo::new(
            compio_io::compat::AsyncStream::new(stream),
        ));
        http1::Builder::new()
            .serve_connection(io, service)
            .await
            .unwrap();
    })
    .detach();
    address
}

fn bad_gateway(error: aioduct::Error) -> Response<Full<Bytes>> {
    Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from(error.to_string())))
        .unwrap()
}

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let upstream = start_upstream().await;
        let broker = start_broker(upstream).await;
        let multipart = Multipart::new().text("model", "ocr-model").part(
            Part::bytes("file", FILE_BYTES.to_vec())
                .file_name("ocr-page.pdf")
                .mime_str("application/pdf"),
        );

        let broker_url = format!("http://{broker}/api/v2/ocr/jobs");
        let response = CompioClient::new()
            .post_local(&broker_url)?
            .multipart(multipart)
            .send()
            .await?;

        println!("status={}", response.status());
        println!("{}", response.text().await?);
        Ok(())
    })
}
