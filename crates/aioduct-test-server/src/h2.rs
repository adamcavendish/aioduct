use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use http_body_util::Full;
use hyper::service::service_fn;
use hyper::{Request, Response};
use tokio::net::TcpListener;

use crate::ConnectionCounter;
use crate::TokioExec;
use crate::h1::HandlerResult;

pub async fn h2_server() -> (SocketAddr, ConnectionCounter) {
    h2_server_with(crate::h1::hello).await
}

pub async fn h2_server_with<F, Fut>(handler: F) -> (SocketAddr, ConnectionCounter)
where
    F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Clone + 'static,
    Fut: Future<Output = HandlerResult> + Send + 'static,
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
                let _ = hyper::server::conn::http2::Builder::new(TokioExec)
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

pub async fn h2_goaway_after(n: usize) -> (SocketAddr, ConnectionCounter) {
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
            let request_count = Arc::new(AtomicUsize::new(0));
            let counter3 = counter2.clone();
            let target = n;
            tokio::spawn(async move {
                let req_count = request_count.clone();
                let conn = hyper::server::conn::http2::Builder::new(TokioExec).serve_connection(
                    io,
                    service_fn(move |_req| {
                        counter3.inc_requests();
                        req_count.fetch_add(1, Ordering::SeqCst);
                        async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok")))) }
                    }),
                );
                tokio::pin!(conn);

                loop {
                    tokio::select! {
                        result = &mut conn => {
                            let _ = result;
                            break;
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                            if request_count.load(Ordering::SeqCst) >= target {
                                conn.as_mut().graceful_shutdown();
                            }
                        }
                    }
                }
            });
        }
    });

    (addr, counter)
}

pub async fn h2_goaway_immediate() -> (SocketAddr, ConnectionCounter) {
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
            let counter3 = counter2.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |_req| {
                    counter3.inc_requests();
                    async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok")))) }
                });
                let conn =
                    hyper::server::conn::http2::Builder::new(TokioExec).serve_connection(io, svc);
                tokio::pin!(conn);
                let _ = (&mut conn).await;
                conn.as_mut().graceful_shutdown();
                let _ = conn.await;
            });
        }
    });

    (addr, counter)
}
