#![allow(dead_code, unused_imports)]

#[macro_use]
pub mod multi_runtime;

pub use std::convert::Infallible;
pub use std::net::SocketAddr;
pub use std::sync::Arc;
pub use std::sync::atomic::{AtomicU32, Ordering};
pub use std::time::Duration;

pub use bytes::Bytes;
pub use http_body_util::Full;
pub use hyper::server::conn::http1 as server_http1;
pub use hyper::service::service_fn;
pub use hyper::{Request, Response};
pub use tokio::net::TcpListener;

pub use aioduct::HttpEngineSend;
pub use aioduct::runtime::TokioRuntime;
pub use aioduct::runtime::tokio_rt::TcpConnector;

#[cfg(feature = "rustls")]
pub fn rustls_crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls_crypto_provider_value())
}

#[cfg(feature = "rustls")]
pub fn install_rustls_crypto_provider() {
    let _ = rustls_crypto_provider_value().install_default();
}

#[cfg(feature = "rustls")]
pub fn rustls_crypto_provider_value() -> rustls::crypto::CryptoProvider {
    #[cfg(feature = "rustls-aws-lc-rs")]
    {
        rustls::crypto::aws_lc_rs::default_provider()
    }

    #[cfg(all(not(feature = "rustls-aws-lc-rs"), feature = "rustls-ring"))]
    {
        rustls::crypto::ring::default_provider()
    }
}

pub async fn hello(
    _req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello aioduct"))))
}

pub async fn echo_headers(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let host = req
        .headers()
        .get("host")
        .map(|v| v.to_str().unwrap_or(""))
        .unwrap_or("missing");
    let path = req.uri().path().to_string();
    let body = format!("host={host}\npath={path}");
    Ok(Response::new(Full::new(Bytes::from(body))))
}

pub async fn start_server() -> SocketAddr {
    start_server_with(|req| async { hello(req).await }).await
}

pub async fn start_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            let handler = handler.clone();
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(io, service_fn(handler))
                    .await;
            });
        }
    });

    addr
}

pub async fn start_h2_server_with<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send + 'static,
{
    use hyper::server::conn::http2 as server_http2;

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

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let io = aioduct::runtime::tokio_rt::TokioIo::new(stream);
            let handler = handler.clone();
            tokio::spawn(async move {
                let _ = server_http2::Builder::new(TokioExec)
                    .serve_connection(io, service_fn(handler))
                    .await;
            });
        }
    });

    addr
}

/// Start a raw TCP server that writes pre-built bytes for each connection.
///
/// The handler receives the raw request bytes (read until the end of headers)
/// and returns the raw response bytes to write back.
pub async fn start_raw_server<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Vec<u8>) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = Vec<u8>> + Send,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                buf.truncate(n);
                let response = handler(buf).await;
                let _ = stream.write_all(&response).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    addr
}

/// Start a raw TCP server where the handler gets direct socket access.
///
/// Useful for tests that need to write partial responses with delays
/// (e.g., fragmented chunked encoding, slow headers).
pub async fn start_raw_streaming_server<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Vec<u8>, tokio::net::TcpStream) -> Fut + Send + Clone + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    use tokio::io::AsyncReadExt;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                buf.truncate(n);
                handler(buf, stream).await;
            });
        }
    });

    addr
}
