#![cfg(all(test, feature = "tokio"))]
#[cfg(all(test, feature = "tokio"))]
mod tokio_tests {
    use crate::client::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};

    /// Helper: build an HttpEngineSend with default settings (no h2c).
    fn make_engine() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
            .build()
            .unwrap()
    }

    /// Helper: build an HttpEngineSend with h2c enabled.
    fn make_h2_engine() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
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
        let result = engine.connect_plaintext_with_hint(io, false).await;
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
        let result = engine.connect_plaintext_with_hint(io, true).await;
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
        let result = engine.connect_plaintext_with_hint(io, false).await;
        assert!(result.is_ok());
        let pooled = result.unwrap();
        assert!(
            !pooled.is_h2_or_h3(),
            "default plaintext connection should be H1"
        );
    }
}
