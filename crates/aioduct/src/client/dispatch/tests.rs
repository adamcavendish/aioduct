use super::*;
use crate::body::RequestBodySend;

type Core = HttpEngineCore<RequestBodySend>;

#[test]
fn is_stale_io_connection_reset() {
    let err = Error::Io(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "reset by peer",
    ));
    assert!(Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_io_broken_pipe() {
    let err = Error::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    ));
    assert!(Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_io_connection_aborted() {
    let err = Error::Io(std::io::Error::new(
        std::io::ErrorKind::ConnectionAborted,
        "connection aborted",
    ));
    assert!(Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_io_other_kind_not_stale() {
    let err = Error::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "timed out",
    ));
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_io_permission_denied_not_stale() {
    let err = Error::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "permission denied",
    ));
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_timeout_not_stale() {
    let err = Error::Timeout;
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_connect_timeout_not_stale() {
    let err = Error::ConnectTimeout;
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_status_not_stale() {
    let err = Error::Status(http::StatusCode::NOT_FOUND);
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_invalid_url_not_stale() {
    let err = Error::InvalidUrl("bad url".into());
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_redirect_not_stale() {
    let err = Error::Redirect("missing location".into());
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_too_many_redirects_not_stale() {
    let err = Error::TooManyRedirects(10);
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_tls_not_stale() {
    let err = Error::Tls("bad cert".into());
    assert!(!Core::is_stale_connection_error_pub(&err));
}

#[test]
fn is_stale_other_not_stale() {
    let err = Error::Other("misc error".into());
    assert!(!Core::is_stale_connection_error_pub(&err));
}

/// A hyper parse error (e.g., malformed response) is not a stale connection error.
/// This exercises the path where a Hyper error is not canceled/closed/incomplete
/// and has no IO source, returning false (dispatch.rs line ~110).
#[cfg(feature = "tokio")]
#[tokio::test]
async fn is_stale_hyper_parse_error_not_stale() {
    use tokio::io::AsyncWriteExt;
    // Create a duplex stream and send garbage HTTP response data
    let (client_io, mut server_io) = tokio::io::duplex(1024);
    let io = crate::runtime::tokio_rt::TokioIo::new(client_io);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = conn.await;
    });

    // Send garbage that will cause a parse error
    tokio::spawn(async move {
        server_io.write_all(b"NOT HTTP AT ALL\r\n\r\n").await.ok();
        server_io.shutdown().await.ok();
    });

    let req = http::Request::builder()
        .uri("http://example.com/")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();

    let result = sender.send_request(req).await;
    if let Err(hyper_err) = result {
        let err = Error::Hyper(hyper_err);
        // Parse errors should NOT be treated as stale connection errors
        assert!(
            !Core::is_stale_connection_error_pub(&err),
            "hyper parse error should not be considered stale; error: {err:?}"
        );
    }
}

// --- connection_protocol tests ---

#[cfg(feature = "tokio")]
#[tokio::test]
async fn connection_protocol_returns_h1_for_h1_connection() {
    use crate::pool::PooledConnection;
    use crate::runtime::tokio_rt::TokioIo;

    let (client_io, mut server_io) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1024];
        loop {
            match server_io.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
        }
    });

    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let pooled = PooledConnection::new_h1(sender);
    assert_eq!(
        Core::connection_protocol(&pooled),
        observer::NegotiatedProtocol::Http1,
    );
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn connection_protocol_returns_h2_for_h2_connection() {
    use crate::pool::PooledConnection;
    use crate::runtime::tokio_rt::{TokioIo, TokioRuntime};

    let (client_io, server_io) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let io = TokioIo::new(server_io);
        let builder =
            hyper::server::conn::http2::Builder::new(crate::runtime::executor::poll_executor::<
                TokioRuntime,
            >());
        let _ = builder
            .serve_connection(
                io,
                hyper::service::service_fn(|_req| async {
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        http_body_util::Empty::<bytes::Bytes>::new(),
                    ))
                }),
            )
            .await;
    });

    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http2::handshake(
        crate::runtime::executor::poll_executor::<TokioRuntime>(),
        io,
    )
    .await
    .expect("h2 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let pooled = PooledConnection::new_h2(sender);
    assert_eq!(
        Core::connection_protocol(&pooled),
        observer::NegotiatedProtocol::Http2,
    );
}

// --- notify / attach_observer / fire_connection_metrics tests ---

#[cfg(feature = "tokio")]
#[tokio::test]
async fn notify_does_nothing_without_observer() {
    use crate::client::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};

    // Build engine without observer - notify should not panic
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();
    let method = http::Method::GET;
    let uri: http::Uri = "http://example.com/".parse().unwrap();
    // Should not panic - observer is None
    engine
        .core
        .notify(&method, &uri, observer::RequestPhase::Started);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn notify_fires_event_with_observer() {
    use crate::client::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone)]
    struct CountingObserver {
        count: Arc<AtomicU32>,
    }
    impl observer::RequestObserver for CountingObserver {
        fn on_event(&self, _event: &observer::RequestEvent) {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
        fn on_connection_event(&self, _event: &observer::ConnectionEvent) {}
    }

    let count = Arc::new(AtomicU32::new(0));
    let obs = CountingObserver {
        count: count.clone(),
    };
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs)
        .build()
        .unwrap();
    let method = http::Method::GET;
    let uri: http::Uri = "http://example.com/".parse().unwrap();
    engine
        .core
        .notify(&method, &uri, observer::RequestPhase::Started);
    assert_eq!(count.load(Ordering::Relaxed), 1);
    engine
        .core
        .notify(&method, &uri, observer::RequestPhase::Started);
    assert_eq!(count.load(Ordering::Relaxed), 2);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn fire_connection_metrics_does_nothing_without_observer() {
    use crate::client::HttpEngineSend;
    use crate::pool::PooledConnection;
    use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};

    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .build()
        .unwrap();

    let (client_io, mut server_io) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1024];
        loop {
            match server_io.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
        }
    });
    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let pooled = PooledConnection::new_h1(sender);
    // Should not panic - observer is None
    engine.core.fire_connection_metrics(&pooled, false);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn fire_connection_metrics_fires_with_observer_and_remote_addr() {
    use crate::client::HttpEngineSend;
    use crate::pool::PooledConnection;
    use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone)]
    struct ConnObserver {
        conn_events: Arc<AtomicU32>,
    }
    impl observer::RequestObserver for ConnObserver {
        fn on_event(&self, _event: &observer::RequestEvent) {}
        fn on_connection_event(&self, _event: &observer::ConnectionEvent) {
            self.conn_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    let conn_events = Arc::new(AtomicU32::new(0));
    let obs = ConnObserver {
        conn_events: conn_events.clone(),
    };
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs)
        .build()
        .unwrap();

    let (client_io, mut server_io) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1024];
        loop {
            match server_io.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
        }
    });
    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut pooled = PooledConnection::new_h1(sender);
    pooled.remote_addr = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 8080)));
    pooled.bytes_sent = 100;
    pooled.bytes_received = 500;
    pooled.requests_served = 3;

    engine.core.fire_connection_metrics(&pooled, false);
    assert_eq!(conn_events.load(Ordering::Relaxed), 1);

    engine.core.fire_connection_metrics(&pooled, true);
    assert_eq!(conn_events.load(Ordering::Relaxed), 2);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn fire_connection_metrics_no_remote_addr_skips() {
    use crate::client::HttpEngineSend;
    use crate::pool::PooledConnection;
    use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone)]
    struct ConnObserver {
        conn_events: Arc<AtomicU32>,
    }
    impl observer::RequestObserver for ConnObserver {
        fn on_event(&self, _event: &observer::RequestEvent) {}
        fn on_connection_event(&self, _event: &observer::ConnectionEvent) {
            self.conn_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    let conn_events = Arc::new(AtomicU32::new(0));
    let obs = ConnObserver {
        conn_events: conn_events.clone(),
    };
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs)
        .build()
        .unwrap();

    let (client_io, mut server_io) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1024];
        loop {
            match server_io.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
        }
    });
    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let pooled = PooledConnection::new_h1(sender);
    // remote_addr is None, so observer should NOT be called
    engine.core.fire_connection_metrics(&pooled, false);
    assert_eq!(conn_events.load(Ordering::Relaxed), 0);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn checkin_connection_multiplex_clone_fires_metrics_but_skips_pool() {
    use crate::client::HttpEngineSend;
    use crate::pool::{PoolKey, PooledConnection};
    use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Clone)]
    struct ConnObserver {
        conn_events: Arc<AtomicU32>,
    }
    impl observer::RequestObserver for ConnObserver {
        fn on_event(&self, _event: &observer::RequestEvent) {}
        fn on_connection_event(&self, _event: &observer::ConnectionEvent) {
            self.conn_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    let conn_events = Arc::new(AtomicU32::new(0));
    let obs = ConnObserver {
        conn_events: conn_events.clone(),
    };
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .request_observer(obs)
        .build()
        .unwrap();

    let (client_io, server_io) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let io = TokioIo::new(server_io);
        let builder =
            hyper::server::conn::http2::Builder::new(crate::runtime::executor::poll_executor::<
                TokioRuntime,
            >());
        let _ = builder
            .serve_connection(
                io,
                hyper::service::service_fn(|_req| async {
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        http_body_util::Empty::<bytes::Bytes>::new(),
                    ))
                }),
            )
            .await;
    });

    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http2::handshake(
        crate::runtime::executor::poll_executor::<TokioRuntime>(),
        io,
    )
    .await
    .expect("h2 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut pooled = PooledConnection::new_h2(sender);
    pooled.remote_addr = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 443)));

    // Clone for multiplex
    let cloned = pooled.clone_for_multiplex().expect("h2 should clone");
    assert!(cloned.is_multiplex_clone);

    let pool_key = PoolKey::new(http::uri::Scheme::HTTPS, "example.com:443".parse().unwrap());

    // Checkin a multiplex clone: should fire metrics but not add to pool
    engine.core.checkin_connection(pool_key.clone(), cloned);
    assert_eq!(conn_events.load(Ordering::Relaxed), 1);

    // Pool should be empty since multiplex clones are not checked in
    assert!(engine.core.pool.checkout(&pool_key).is_none());
}

/// A hyper error whose source is an IO error with ConnectionReset is a stale connection error.
/// This exercises the path where a Hyper error has an io::Error source (dispatch.rs lines ~103-107).
#[cfg(feature = "tokio")]
#[tokio::test]
async fn is_stale_hyper_with_io_source_connection_reset() {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    /// A mock IO stream that returns ConnectionReset on reads.
    struct ResetOnRead;

    impl AsyncRead for ResetOnRead {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset",
            )))
        }
    }

    impl AsyncWrite for ResetOnRead {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl Unpin for ResetOnRead {}

    let io = crate::runtime::tokio_rt::TokioIo::new(ResetOnRead);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("handshake");

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = http::Request::builder()
        .uri("http://example.com/")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .unwrap();

    let result = sender.send_request(req).await;
    if let Err(hyper_err) = result {
        let err = Error::Hyper(hyper_err);
        // Check that the error IS stale (connection reset wrapped in hyper)
        // OR the hyper error manifests as canceled/closed (implementation detail).
        // Either way, it should be detected as stale.
        assert!(
            Core::is_stale_connection_error_pub(&err),
            "hyper error with IO ConnectionReset source should be stale; error: {err:?}"
        );
    }
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn bytes_sent_uses_content_length_header_for_streaming_body() {
    use crate::pool::PooledConnection;
    use crate::runtime::tokio_rt::TokioIo;
    use http_body_util::BodyExt;

    let (client_io, server_io) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let io = TokioIo::new(server_io);
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                io,
                hyper::service::service_fn(|_req| async {
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        http_body_util::Empty::<bytes::Bytes>::new(),
                    ))
                }),
            )
            .await;
    });

    let io = TokioIo::new(client_io);
    let (sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("h1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut pooled = PooledConnection::new_h1(sender);

    let stream_body = futures_util::stream::iter(vec![Ok::<_, crate::error::Error>(
        hyper::body::Frame::data(bytes::Bytes::from("hello streaming world")),
    )]);
    let body: RequestBodySend = http_body_util::StreamBody::new(stream_body).boxed_unsync();

    assert!(
        http_body::Body::size_hint(&body).exact().is_none(),
        "streaming body should not have exact size hint"
    );

    let request = http::Request::post("/upload")
        .header("content-length", "21")
        .header("host", "example.com")
        .body(body)
        .unwrap();

    let uri: http::Uri = "http://example.com/upload".parse().unwrap();
    let _ = Core::send_on_connection(&mut pooled, request, uri).await;

    assert_eq!(
        pooled.bytes_sent, 21,
        "bytes_sent should use Content-Length header value for streaming bodies"
    );
}

fn empty_response(status: u16, headers: &[(&str, &str)]) -> Response {
    use http_body_util::BodyExt;
    let mut builder = http::Response::builder().status(status);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let body: crate::body::RequestBodySend = http_body_util::Empty::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let inner = builder
        .body(crate::response::ResponseBodySend::from_boxed(body))
        .unwrap();
    Response::new(inner, "http://example.com/".parse().unwrap())
}

#[test]
fn should_skip_checkin_switching_protocols() {
    let resp = empty_response(101, &[]);
    assert!(Core::should_skip_checkin(&resp));
}

#[test]
fn should_skip_checkin_connection_close() {
    let resp = empty_response(200, &[("connection", "close")]);
    assert!(Core::should_skip_checkin(&resp));
}

#[test]
fn should_skip_checkin_connection_close_case_insensitive() {
    let resp = empty_response(200, &[("connection", "Close")]);
    assert!(Core::should_skip_checkin(&resp));
}

#[test]
fn should_not_skip_checkin_normal_response() {
    let resp = empty_response(200, &[]);
    assert!(!Core::should_skip_checkin(&resp));
}

#[test]
fn should_not_skip_checkin_keepalive() {
    let resp = empty_response(200, &[("connection", "keep-alive")]);
    assert!(!Core::should_skip_checkin(&resp));
}
