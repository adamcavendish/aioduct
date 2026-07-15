#[cfg(test)]
mod tests {
    use crate::client::connect_handshake::parse_connect_response;

    #[test]
    fn parses_http10_and_http11_responses() {
        for version in ["HTTP/1.0", "HTTP/1.1"] {
            let response = format!("{version} 200 Connection Established\r\nX-Proxy: yes\r\n\r\n");
            assert_eq!(parse_connect_response(response.as_bytes()).unwrap(), 200);
        }
    }

    #[test]
    fn rejects_malformed_status_and_header_framing() {
        for response in [
            &b""[..],
            &b"garbage\r\n\r\n"[..],
            &b"HTTP/1.1 abc Forbidden\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\n\n"[..],
            &b"HTTP/1.1 200 OK\r\ninvalid header\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nBad Header: value\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nX-Test: value\r\n"[..],
            &b"HTTP/1.1 200 OK\r\n\r\nextra"[..],
        ] {
            assert!(
                parse_connect_response(response).is_err(),
                "accepted malformed response: {:?}",
                String::from_utf8_lossy(response)
            );
        }
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tokio_tests {
    use std::cell::Cell;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use crate::client::connect_handshake::do_connect_handshake;

    async fn handshake_with_response(response: Vec<u8>) -> Result<(), crate::Error> {
        let (client_io, mut server_io) = tokio::io::duplex(16 * 1024);
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let mut request = [0u8; 4096];
            let _ = server_io.read(&mut request).await.unwrap();
            server_io.write_all(&response).await.unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        do_connect_handshake(stream, &proxy, "target.example.com:443")
            .await
            .map(|_| ())
    }

    // ── do_connect_handshake integration tests ──────────────────────────────

    #[tokio::test]
    async fn do_connect_handshake_succeeds_with_200() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let n = server_io.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.starts_with("CONNECT target.example.com:443"),
                "got: {req}"
            );
            assert!(req.contains("Host: target.example.com:443"), "got: {req}");
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_ok(), "handshake should succeed");
    }

    #[tokio::test]
    async fn do_connect_handshake_accepts_every_2xx_boundary() {
        for status in [200, 201, 204, 299] {
            let response = format!("HTTP/1.1 {status} Established\r\n\r\n").into_bytes();
            let result = handshake_with_response(response).await;
            assert!(result.is_ok(), "status {status} should establish a tunnel");
        }
    }

    #[tokio::test]
    async fn do_connect_handshake_consumes_informational_responses_before_success() {
        let response = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nLink: </style.css>\r\n\r\nHTTP/1.1 199 Informational Boundary\r\n\r\nHTTP/1.1 201 Established\r\n\r\n".to_vec();

        handshake_with_response(response).await.unwrap();
    }

    #[tokio::test]
    async fn do_connect_handshake_bounds_informational_responses() {
        let mut response = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(17);
        response.extend_from_slice(b"HTTP/1.1 200 Established\r\n\r\n");

        let error = handshake_with_response(response).await.unwrap_err();
        assert!(
            error.to_string().contains("too many informational"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn do_connect_handshake_rejects_101_without_waiting_for_another_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client_io, mut server_io) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let mut request = [0u8; 4096];
            let _ = server_io.read(&mut request).await.unwrap();
            server_io
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: proxy-tunnel\r\n\r\n",
                )
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            do_connect_handshake(stream, &proxy, "target.example.com:443"),
        )
        .await
        .expect("101 must be rejected without waiting for another response");
        let error = match result {
            Ok(_) => panic!("101 unexpectedly established a CONNECT tunnel"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("101"), "{error}");
    }

    #[tokio::test]
    async fn informational_connect_responses_leave_tunnel_bytes_unread() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client_io, mut server_io) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let mut request = [0u8; 4096];
            let _ = server_io.read(&mut request).await.unwrap();
            server_io
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 Established\r\n\r\nX")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let stream = do_connect_handshake(stream, &proxy, "target.example.com:443")
            .await
            .unwrap();
        let mut inner = stream.into_inner();
        let mut tunnel_byte = [0u8; 1];
        inner.read_exact(&mut tunnel_byte).await.unwrap();

        assert_eq!(tunnel_byte, *b"X");
    }

    #[tokio::test]
    async fn do_connect_handshake_accepts_http10_success() {
        let result =
            handshake_with_response(b"HTTP/1.0 200 Connection Established\r\n\r\n".to_vec()).await;

        assert!(
            result.is_ok(),
            "HTTP/1.0 proxies may establish CONNECT tunnels"
        );
    }

    #[tokio::test]
    async fn do_connect_handshake_rejects_non_2xx_boundaries() {
        for status in [300, 407, 500] {
            let response = format!("HTTP/1.1 {status} Rejected\r\n\r\n").into_bytes();
            let error = handshake_with_response(response).await.unwrap_err();
            assert!(
                error.to_string().contains(&status.to_string()),
                "status {status} was not preserved in {error}"
            );
        }
    }

    #[tokio::test]
    async fn do_connect_handshake_leaves_first_tunnel_byte_unread() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client_io, mut server_io) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let mut request = [0u8; 4096];
            let _ = server_io.read(&mut request).await.unwrap();
            server_io
                .write_all(b"HTTP/1.1 200 Established\r\n\r\nX")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let stream = do_connect_handshake(stream, &proxy, "target.example.com:443")
            .await
            .unwrap();
        let mut inner = stream.into_inner();
        let mut tunnel_byte = [0u8; 1];
        inner.read_exact(&mut tunnel_byte).await.unwrap();

        assert_eq!(tunnel_byte, *b"X");
    }

    #[tokio::test]
    async fn do_connect_handshake_rejects_excessive_response_head() {
        let response =
            format!("HTTP/1.1 200 OK\r\nX-Padding: {}\r\n\r\n", "a".repeat(8192)).into_bytes();
        let error = handshake_with_response(response).await.unwrap_err();

        assert!(error.to_string().contains("too large"), "{error}");
    }

    #[tokio::test]
    async fn do_connect_handshake_rejects_excessive_header_count() {
        let mut response = b"HTTP/1.1 200 OK\r\n".to_vec();
        for index in 0..65 {
            response.extend_from_slice(format!("X-{index}: value\r\n").as_bytes());
        }
        response.extend_from_slice(b"\r\n");

        let error = handshake_with_response(response).await.unwrap_err();
        assert!(error.to_string().contains("too many headers"), "{error}");
    }

    #[tokio::test]
    async fn do_connect_handshake_fails_on_407() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let _ = server_io.read(&mut buf).await.unwrap();
            server_io
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(err.contains("407"), "error should contain 407, got: {err}");
    }

    #[tokio::test]
    async fn do_connect_handshake_fails_on_malformed_response() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            server_io
                .write_all(b"garbage without status\r\n\r\n")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn do_connect_handshake_includes_proxy_auth() {
        let (client_io, mut server_io) = tokio::io::duplex(8192);
        let target = "target.example.com:443".to_string();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let n = server_io.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains("Proxy-Authorization:"),
                "should include auth header, got: {req}"
            );
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
        });

        let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080")
            .unwrap()
            .basic_auth("user", "pass");
        let stream = crate::runtime::tokio_rt::TokioIo::new(client_io);
        let result = do_connect_handshake(stream, &proxy, &target).await;
        assert!(result.is_ok());
    }

    // ── poll_flush ordering tests ───────────────────────────────────────────

    /// Mock stream that records whether poll_flush was called before poll_read.
    struct FlushTrackingStream {
        response: Vec<u8>,
        read_pos: usize,
        flushed: Cell<bool>,
    }

    impl FlushTrackingStream {
        fn new(response: &[u8]) -> Self {
            Self {
                response: response.to_vec(),
                read_pos: 0,
                flushed: Cell::new(false),
            }
        }
        fn was_flushed(&self) -> bool {
            self.flushed.get()
        }
    }

    impl hyper::rt::Write for FlushTrackingStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.flushed.set(true);
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Read for FlushTrackingStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<std::io::Result<()>> {
            let this = self.get_mut();
            // Assert flush was called before any read
            assert!(
                this.flushed.get(),
                "poll_flush must be called before poll_read"
            );
            if this.read_pos < this.response.len() {
                let remaining = &this.response[this.read_pos..];
                let to_copy = remaining.len().min(buf.remaining());
                let dest = unsafe { buf.as_mut() };
                // Manually copy from initialized bytes into MaybeUninit buffer
                for (i, &byte) in remaining[..to_copy].iter().enumerate() {
                    dest[i].write(byte);
                }
                unsafe { buf.advance(to_copy) };
                this.read_pos += to_copy;
            }
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn connect_handshake_flushes_before_read() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();
            let response = b"HTTP/1.1 200 Connection Established\r\n\r\n";
            let stream = FlushTrackingStream::new(response);

            let result = do_connect_handshake(stream, &proxy, "example.com:80").await;
            assert!(
                result.is_ok(),
                "handshake should succeed: {:?}",
                result.err()
            );
            assert!(
                result.unwrap().was_flushed(),
                "stream should have been flushed"
            );
        });
    }

    #[test]
    fn connect_handshake_flush_failure_propagates() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let proxy = crate::proxy::ProxyConfig::http("http://proxy:8080").unwrap();

            // Stream that fails on flush
            #[derive(Debug)]
            struct FlushFailingStream;
            impl hyper::rt::Write for FlushFailingStream {
                fn poll_write(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                    buf: &[u8],
                ) -> Poll<std::io::Result<usize>> {
                    Poll::Ready(Ok(buf.len()))
                }
                fn poll_flush(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                ) -> Poll<std::io::Result<()>> {
                    Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "flush failed",
                    )))
                }
                fn poll_shutdown(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                ) -> Poll<std::io::Result<()>> {
                    Poll::Ready(Ok(()))
                }
            }
            impl hyper::rt::Read for FlushFailingStream {
                fn poll_read(
                    self: Pin<&mut Self>,
                    _cx: &mut Context<'_>,
                    _buf: hyper::rt::ReadBufCursor<'_>,
                ) -> Poll<std::io::Result<()>> {
                    Poll::Ready(Ok(()))
                }
            }

            let result = do_connect_handshake(FlushFailingStream, &proxy, "example.com:80").await;
            assert!(result.is_err());
            assert!(
                format!("{}", result.unwrap_err()).contains("flush"),
                "error should mention flush failure"
            );
        });
    }
}
