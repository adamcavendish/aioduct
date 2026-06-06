use std::future::Future;
use std::pin::Pin;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::pool::PooledConnection;
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::HttpEngineSend;

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    pub(super) fn connect_plaintext<S>(
        &self,
        stream: S,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodySend>, Error>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.connect_plaintext_with_hint(stream, false)
    }

    pub(super) fn connect_plaintext_with_hint<S>(
        &self,
        stream: S,
        force_h2c: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodySend>, Error>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        if self.core.http2_prior_knowledge || force_h2c {
            Box::pin(self.connect_h2_prior_knowledge(stream))
        } else {
            Box::pin(self.connect_h1(stream))
        }
    }

    pub(super) async fn connect_h1<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let (sender, conn) = hyper::client::conn::http1::handshake(stream).await?;
        R::spawn_send(async move {
            let _ = conn.with_upgrades().await;
        });
        Ok(PooledConnection::new_h1(sender))
    }

    pub(super) async fn connect_h2_prior_knowledge<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let mut builder = hyper::client::conn::http2::Builder::new(
            crate::runtime::executor::poll_executor::<R>(),
        );
        if let Some(ref h2) = self.core.http2 {
            h2.apply(&mut builder);
        }
        let (sender, conn) = builder.handshake(stream).await?;
        R::spawn_send(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h2(sender))
    }

    #[cfg(feature = "rustls")]
    pub(super) async fn connect_tls(
        &self,
        tcp_stream: C::Stream,
        host: &str,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        use crate::tls::TlsConnect;
        use std::time::Instant;

        #[cfg(feature = "tracing")]
        tracing::trace!(host = host, "tls.handshake.start");

        let tls_start = Instant::now();

        let tls_connector = self
            .core
            .tls
            .as_ref()
            .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

        let tls_stream = <crate::tls::RustlsConnector as TlsConnect<C::Stream>>::connect(
            tls_connector,
            host,
            tcp_stream,
        )
        .await
        .map_err(|e| {
            #[cfg(feature = "tracing")]
            tracing::trace!(host = host, error = %e, "tls.handshake.error");
            Error::Tls(Box::new(e))
        })?;

        let tls_duration = tls_start.elapsed();

        let alpn = crate::tls::RustlsConnector::negotiated_protocol(tls_stream.tls_connection());

        #[cfg(feature = "tracing")]
        tracing::trace!(
            host = host,
            alpn = ?alpn,
            "tls.handshake.done",
        );
        let tls_info = tls_stream.tls_info();

        match alpn {
            Some(crate::tls::AlpnProtocol::H2) => {
                let mut builder = hyper::client::conn::http2::Builder::new(
                    crate::runtime::executor::poll_executor::<R>(),
                );
                if let Some(ref h2) = self.core.http2 {
                    h2.apply(&mut builder);
                }
                let (sender, conn) = builder.handshake(tls_stream).await?;
                R::spawn_send(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h2(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn_send(async move {
                    let _ = conn.with_upgrades().await;
                });
                let mut pooled = PooledConnection::new_h1(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
        }
    }

    #[cfg(not(feature = "rustls"))]
    pub(super) async fn connect_tls(
        &self,
        _tcp_stream: C::Stream,
        _host: &str,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        Err(Error::Tls(
            "HTTPS requires the `rustls` TLS backend feature".into(),
        ))
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tokio_tests {
    use super::super::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};

    /// Helper: build an HttpEngineSend with default http2_prior_knowledge = false.
    fn make_engine() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap()
    }

    /// Helper: build an HttpEngineSend with http2_prior_knowledge = true.
    fn make_h2_engine() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .http2_prior_knowledge()
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn connect_h1_succeeds_with_duplex() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);

        // Keep server alive to allow h1 handshake
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            loop {
                match server_io.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
            }
        });

        let io = TokioIo::new(client_io);
        let engine = make_engine();
        let result = engine.connect_h1(io).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        // Verify it's an H1 connection
        assert!(matches!(pooled.conn, crate::pool::HttpConnection::H1(_)));
    }

    #[tokio::test]
    async fn connect_h2_prior_knowledge_succeeds_with_duplex() {
        let (client_io, server_io) = tokio::io::duplex(65536);

        // Spawn an h2 server that accepts the connection
        tokio::spawn(async move {
            let io = TokioIo::new(server_io);
            let builder = hyper::server::conn::http2::Builder::new(
                crate::runtime::executor::poll_executor::<TokioRuntime>(),
            );
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
        let engine = make_engine();
        let result = engine.connect_h2_prior_knowledge(io).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        // Verify it's an H2 connection
        assert!(matches!(pooled.conn, crate::pool::HttpConnection::H2(_)));
    }

    #[tokio::test]
    async fn connect_plaintext_defaults_to_h1() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);

        // Keep server alive
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            loop {
                match server_io.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
            }
        });

        let io = TokioIo::new(client_io);
        let engine = make_engine();
        let result = engine.connect_plaintext(io).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        assert!(matches!(pooled.conn, crate::pool::HttpConnection::H1(_)));
    }

    #[tokio::test]
    async fn connect_plaintext_with_hint_false_uses_h1() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);

        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            loop {
                match server_io.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
            }
        });

        let io = TokioIo::new(client_io);
        let engine = make_engine();
        let result = engine.connect_plaintext_with_hint(io, false).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        assert!(matches!(pooled.conn, crate::pool::HttpConnection::H1(_)));
    }

    #[tokio::test]
    async fn connect_plaintext_with_hint_true_uses_h2() {
        let (client_io, server_io) = tokio::io::duplex(65536);

        // Spawn an h2 server
        tokio::spawn(async move {
            let io = TokioIo::new(server_io);
            let builder = hyper::server::conn::http2::Builder::new(
                crate::runtime::executor::poll_executor::<TokioRuntime>(),
            );
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
        let engine = make_engine();
        let result = engine.connect_plaintext_with_hint(io, true).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        assert!(matches!(pooled.conn, crate::pool::HttpConnection::H2(_)));
    }

    #[tokio::test]
    async fn connect_plaintext_with_http2_prior_knowledge_uses_h2() {
        let (client_io, server_io) = tokio::io::duplex(65536);

        // Spawn an h2 server
        tokio::spawn(async move {
            let io = TokioIo::new(server_io);
            let builder = hyper::server::conn::http2::Builder::new(
                crate::runtime::executor::poll_executor::<TokioRuntime>(),
            );
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
        let engine = make_h2_engine();
        // http2_prior_knowledge = true means connect_plaintext should use h2
        let result = engine.connect_plaintext(io).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        assert!(matches!(pooled.conn, crate::pool::HttpConnection::H2(_)));
    }

    #[tokio::test]
    async fn connect_h1_server_closes_immediately() {
        let (client_io, server_io) = tokio::io::duplex(8192);
        // Drop server immediately — handshake should still succeed because
        // hyper's h1 handshake doesn't require server data.
        drop(server_io);

        let io = TokioIo::new(client_io);
        let engine = make_engine();
        let result = engine.connect_h1(io).await;
        // h1 handshake does not require server response — it just creates the sender
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn connect_h2_server_closes_immediately_fails() {
        let (client_io, server_io) = tokio::io::duplex(8192);
        // Drop server immediately — h2 handshake requires preface exchange
        drop(server_io);

        let io = TokioIo::new(client_io);
        let engine = make_engine();
        let result = engine.connect_h2_prior_knowledge(io).await;
        // h2 handshake needs server preface; will fail with closed connection
        assert!(result.is_err());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tls_on_plain_tcp_stream_fails() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncWriteExt;
            let _ = conn.write_all(b"this is not TLS").await;
            let _ = conn.shutdown().await;
        });

        let engine = make_engine();
        let connector = TcpConnector;
        let stream = <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, addr)
            .await
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine.connect_tls(stream, "example.com"),
        )
        .await
        .expect("tls handshake should complete within timeout");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connect_plaintext_returns_h1_by_default() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);

        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            loop {
                match server_io.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
            }
        });

        let io = TokioIo::new(client_io);
        let engine = make_engine();
        // connect_plaintext with default engine (no http2_prior_knowledge) should use H1
        let result = engine.connect_plaintext(io).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        assert!(
            !pooled.is_h2_or_h3(),
            "default plaintext connection should be H1"
        );
    }
}
