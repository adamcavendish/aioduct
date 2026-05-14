use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::ConnectionCounter;

pub type HandlerResult = Result<Response<Full<Bytes>>, Infallible>;

pub async fn hello(_req: Request<hyper::body::Incoming>) -> HandlerResult {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

pub async fn echo(req: Request<hyper::body::Incoming>) -> HandlerResult {
    use http_body_util::BodyExt;

    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let headers = req
        .headers()
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v.to_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n");
    let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
    let body = format!(
        "method={method}\npath={path}\nheaders:\n{headers}\nbody={body}",
        body = String::from_utf8_lossy(&body_bytes)
    );
    Ok(Response::new(Full::new(Bytes::from(body))))
}

pub async fn echo_headers(req: Request<hyper::body::Incoming>) -> HandlerResult {
    let host = req
        .headers()
        .get("host")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("missing");
    let path = req.uri().path().to_string();
    let body = format!("host={host}\npath={path}");
    Ok(Response::new(Full::new(Bytes::from(body))))
}

pub fn http_200_keepalive() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok"
}

pub fn http_200_close() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
}

pub async fn h1_server() -> (SocketAddr, ConnectionCounter) {
    h1_server_with(hello).await
}

pub async fn h1_server_with<F, Fut>(handler: F) -> (SocketAddr, ConnectionCounter)
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: Future<Output = HandlerResult> + Send,
{
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let io = crate::TokioIo::new(stream);
            let handler = handler.clone();
            let counter3 = counter2.clone();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req| {
                            counter3.inc_requests();
                            let handler = handler.clone();
                            async move { handler(req).await }
                        }),
                    )
                    .await;
            });
        }
    });

    (addr, counter)
}

pub async fn h1_echo_server() -> (SocketAddr, ConnectionCounter) {
    h1_server_with(echo).await
}

pub async fn h1_slow_body_server(
    chunk_size: usize,
    chunk_delay: Duration,
) -> (SocketAddr, ConnectionCounter) {
    let counter = ConnectionCounter::new();
    let counter2 = counter.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            counter2.inc_connections();
            let counter3 = counter2.clone();
            tokio::spawn(async move {
                loop {
                    let mut buf = [0u8; 8192];
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        continue;
                    }
                    counter3.inc_requests();

                    let total_chunks = 10usize;
                    let total_size = chunk_size * total_chunks;
                    let header = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n".to_string();
                    let _ = stream.write_all(header.as_bytes()).await;

                    let chunk_data = vec![b'x'; chunk_size];
                    for _ in 0..total_chunks {
                        let chunk_header = format!("{:x}\r\n", chunk_size);
                        let _ = stream.write_all(chunk_header.as_bytes()).await;
                        let _ = stream.write_all(&chunk_data).await;
                        let _ = stream.write_all(b"\r\n").await;
                        let _ = stream.flush().await;
                        tokio::time::sleep(chunk_delay).await;
                    }
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                    let _ = stream.flush().await;
                    let _ = total_size;
                }
            });
        }
    });

    (addr, counter)
}

pub async fn h1_large_body_server(body_size: usize) -> (SocketAddr, ConnectionCounter) {
    h1_server_with(move |_req| {
        let body = vec![b'x'; body_size];
        async move { Ok(Response::new(Full::new(Bytes::from(body)))) }
    })
    .await
}

pub fn spawn_h1_server() -> SocketAddr {
    spawn_h1_server_with(hello)
}

pub fn spawn_h1_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: Future<Output = HandlerResult> + Send,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();

            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let io = crate::TokioIo::new(stream);
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service_fn(handler))
                        .await;
                });
            }
        });
    });
    rx.recv().unwrap()
}
