use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper::server::conn::http2 as server_http2;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

pub const JSON_BODY: &str = r#"{"message":"hello","count":42}"#;
pub const BODY_64K: usize = 64 * 1024;
pub const BODY_1M: usize = 1024 * 1024;
pub const SSE_EVENT_COUNT: usize = 100;

pub async fn start_http1_server(body: Bytes) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            let body = body.clone();
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |_req: Request<hyper::body::Incoming>| {
                            let body = body.clone();
                            async move {
                                Ok::<_, Infallible>(Response::new(Full::new(body)))
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

#[derive(Clone)]
pub struct TokioExec;

impl<F> hyper::rt::Executor<F> for TokioExec
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::spawn(fut);
    }
}

pub async fn start_h2c_server(body: Bytes) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            let body = body.clone();
            tokio::spawn(async move {
                let mut builder = server_http2::Builder::new(TokioExec);
                builder
                    .initial_stream_window_size(2 * 1024 * 1024)
                    .initial_connection_window_size(4 * 1024 * 1024);
                let _ = builder
                    .serve_connection(
                        io,
                        service_fn(move |_req: Request<hyper::body::Incoming>| {
                            let body = body.clone();
                            async move {
                                Ok::<_, Infallible>(Response::new(Full::new(body)))
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

pub async fn start_h2c_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let mut builder = server_http2::Builder::new(TokioExec);
                builder
                    .initial_stream_window_size(2 * 1024 * 1024)
                    .initial_connection_window_size(4 * 1024 * 1024);
                let _ = builder
                    .serve_connection(
                        io,
                        service_fn(|req: Request<hyper::body::Incoming>| async move {
                            use http_body_util::BodyExt;
                            let body = req.into_body().collect().await.unwrap().to_bytes();
                            let resp = Response::builder()
                                .header("content-length", body.len())
                                .body(Full::new(body))
                                .unwrap();
                            Ok::<_, Infallible>(resp)
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

pub async fn start_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(|req: Request<hyper::body::Incoming>| async move {
                            use http_body_util::BodyExt;
                            let body = req.into_body().collect().await.unwrap().to_bytes();
                            let resp = Response::builder()
                                .header("content-length", body.len())
                                .body(Full::new(body))
                                .unwrap();
                            Ok::<_, Infallible>(resp)
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

pub async fn start_sse_server(event_count: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |_req: Request<hyper::body::Incoming>| async move {
                            let mut body = String::new();
                            for i in 0..event_count {
                                body.push_str(&format!(
                                    "event: tick\ndata: {{\"n\":{i}}}\n\n"
                                ));
                            }
                            let resp = Response::builder()
                                .header("content-type", "text/event-stream")
                                .header("cache-control", "no-cache")
                                .body(Full::new(Bytes::from(body)))
                                .unwrap();
                            Ok::<_, Infallible>(resp)
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

pub async fn start_range_server(total_size: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let data: Bytes = Bytes::from(vec![b'A'; total_size]);
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            stream.set_nodelay(true).unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            let data = data.clone();
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req: Request<hyper::body::Incoming>| {
                            let data = data.clone();
                            async move {
                                let total = data.len();
                                if req.method() == hyper::Method::HEAD {
                                    let resp = Response::builder()
                                        .header("content-length", total)
                                        .header("accept-ranges", "bytes")
                                        .body(Full::new(Bytes::new()))
                                        .unwrap();
                                    return Ok::<_, Infallible>(resp);
                                }
                                if let Some(range) = req.headers().get("range") {
                                    let range_str = range.to_str().unwrap_or("");
                                    let range_str =
                                        range_str.strip_prefix("bytes=").unwrap_or(range_str);
                                    let parts: Vec<&str> = range_str.split('-').collect();
                                    let start: usize = parts[0].parse().unwrap_or(0);
                                    let end: usize = parts
                                        .get(1)
                                        .and_then(|s| s.parse().ok())
                                        .unwrap_or(total - 1)
                                        .min(total - 1);
                                    let slice = data.slice(start..=end);
                                    let resp = Response::builder()
                                        .status(206)
                                        .header(
                                            "content-range",
                                            format!("bytes {start}-{end}/{total}"),
                                        )
                                        .header("content-length", slice.len())
                                        .header("accept-ranges", "bytes")
                                        .body(Full::new(slice))
                                        .unwrap();
                                    Ok(resp)
                                } else {
                                    let resp = Response::builder()
                                        .header("content-length", total)
                                        .header("accept-ranges", "bytes")
                                        .body(Full::new(data))
                                        .unwrap();
                                    Ok(resp)
                                }
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}
