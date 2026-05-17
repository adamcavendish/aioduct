use std::future::Future;
use std::pin::Pin;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::pool::PooledConnection;
use crate::proxy::ProxyConfig;
use crate::runtime::{ConnectorSend, RuntimePoll, SocketConfig};

use super::HttpEngineSend;

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    pub(super) async fn connect_via_proxy(
        &self,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        let proxy_authority = proxy.authority()?;
        let default_port = proxy.default_port();
        let proxy_addr = self
            .core
            .resolve_authority(proxy_authority, default_port)
            .await?;
        let tcp_stream = if let Some(local_addr) = self.core.local_address {
            self.connector
                .connect_bound(proxy_addr, local_addr)
                .await
                .map_err(Error::Io)?
        } else {
            self.connector
                .connect(proxy_addr)
                .await
                .map_err(Error::Io)?
        };
        #[cfg(target_os = "linux")]
        if let Some(ref iface) = self.core.interface {
            tcp_stream.bind_device(iface).map_err(Error::Io)?;
        }
        if let Some(time) = self.core.tcp_keepalive {
            tcp_stream
                .set_keepalive(
                    time,
                    self.core.tcp_keepalive_interval,
                    self.core.tcp_keepalive_retries,
                )
                .map_err(Error::Io)?;
        }
        if self.core.tcp_fast_open {
            let _ = tcp_stream.set_fast_open();
        }

        if proxy.scheme == crate::proxy::ProxyScheme::Socks5 {
            let host = target_authority.host();
            let port = target_authority
                .port_u16()
                .unwrap_or(if is_https { 443 } else { 80 });
            let mut std_stream = self.connector.into_std_tcp(tcp_stream).map_err(Error::Io)?;
            crate::socks5::socks5_handshake(&mut std_stream, host, port, proxy.auth.as_ref())
                .map_err(Error::Io)?;
            let tcp_stream = self.connector.from_std_tcp(std_stream).map_err(Error::Io)?;
            if is_https {
                self.connect_tls(tcp_stream, host).await
            } else {
                self.connect_h1(tcp_stream).await
            }
        } else if proxy.scheme == crate::proxy::ProxyScheme::Socks4 {
            let host = target_authority.host();
            let port = target_authority
                .port_u16()
                .unwrap_or(if is_https { 443 } else { 80 });
            let mut std_stream = self.connector.into_std_tcp(tcp_stream).map_err(Error::Io)?;
            crate::socks4::socks4a_handshake(&mut std_stream, host, port, proxy.auth.as_ref())
                .map_err(Error::Io)?;
            let tcp_stream = self.connector.from_std_tcp(std_stream).map_err(Error::Io)?;
            if is_https {
                self.connect_tls(tcp_stream, host).await
            } else {
                self.connect_h1(tcp_stream).await
            }
        } else if is_https {
            self.connect_tunnel(tcp_stream, proxy, target_authority)
                .await
        } else {
            self.connect_plaintext(tcp_stream).await
        }
    }

    async fn connect_tunnel(
        &self,
        mut tcp_stream: C::Stream,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        use hyper::rt::{Read, Write};

        let target = target_authority.as_str();

        let mut connect_msg = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(auth_value) = proxy.connect_header(target) {
            connect_msg.push_str(&format!("Proxy-Authorization: {auth_value}\r\n"));
        }
        connect_msg.push_str("\r\n");

        let buf = connect_msg.into_bytes();
        let mut written = 0;
        while written < buf.len() {
            let n = std::future::poll_fn(|cx| {
                Pin::new(&mut tcp_stream).poll_write(cx, &buf[written..])
            })
            .await
            .map_err(Error::Io)?;
            written += n;
        }

        let mut resp_buf = Vec::with_capacity(256);
        loop {
            let mut one = [0u8; 1];
            let mut read_buf = hyper::rt::ReadBuf::new(&mut one);
            std::future::poll_fn(|cx| Pin::new(&mut tcp_stream).poll_read(cx, read_buf.unfilled()))
                .await
                .map_err(Error::Io)?;

            if read_buf.filled().is_empty() {
                return Err(Error::Other("proxy closed connection".into()));
            }
            resp_buf.push(one[0]);

            if resp_buf.len() >= 4 && resp_buf[resp_buf.len() - 4..] == *b"\r\n\r\n" {
                break;
            }

            if resp_buf.len() > 8192 {
                return Err(Error::Other("CONNECT response too large".into()));
            }
        }

        let resp_str = String::from_utf8_lossy(&resp_buf);
        let status_line = resp_str
            .lines()
            .next()
            .ok_or_else(|| Error::Other("empty CONNECT response".into()))?;

        let status_code = parse_connect_status(status_line)?;
        if status_code != 200 {
            return Err(Error::Other(
                format!("CONNECT tunnel failed: {status_line}").into(),
            ));
        }

        self.connect_tls(tcp_stream, target_authority.host()).await
    }

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
                    let _ = conn.await;
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

pub(super) fn parse_connect_status(status_line: &str) -> Result<u16, Error> {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| Error::Other(format!("malformed CONNECT status line: {status_line}").into()))
}

#[cfg(test)]
mod tests {
    use super::parse_connect_status;

    #[test]
    fn parse_200_ok() {
        assert_eq!(parse_connect_status("HTTP/1.1 200 OK").unwrap(), 200);
    }

    #[test]
    fn parse_200_connection_established() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 200 Connection Established").unwrap(),
            200
        );
    }

    #[test]
    fn parse_407_proxy_auth_required() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 407 Proxy Authentication Required").unwrap(),
            407
        );
    }

    #[test]
    fn parse_403_forbidden() {
        assert_eq!(parse_connect_status("HTTP/1.1 403 Forbidden").unwrap(), 403);
    }

    #[test]
    fn malformed_status_line_returns_error() {
        assert!(parse_connect_status("garbage").is_err());
    }

    #[test]
    fn empty_status_line_returns_error() {
        assert!(parse_connect_status("").is_err());
    }

    #[test]
    fn status_with_200_in_reason_is_not_200() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 403 Contains 200 in text").unwrap(),
            403
        );
    }

    #[test]
    fn parse_non_numeric_status_code_returns_error() {
        assert!(parse_connect_status("HTTP/1.1 abc Forbidden").is_err());
    }

    #[test]
    fn parse_no_second_token_returns_error() {
        assert!(parse_connect_status("HTTP/1.1").is_err());
    }

    #[test]
    fn parse_301_redirect() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 301 Moved Permanently").unwrap(),
            301
        );
    }

    #[test]
    fn parse_503_service_unavailable() {
        assert_eq!(
            parse_connect_status("HTTP/1.1 503 Service Unavailable").unwrap(),
            503
        );
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tokio_tests {
    use super::super::HttpEngineSend;
    use crate::runtime::tokio_rt::{TcpConnector, TokioIo, TokioRuntime};

    /// Helper: build an HttpEngineSend with default http2_prior_knowledge = false.
    fn make_engine() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector).build()
    }

    /// Helper: build an HttpEngineSend with http2_prior_knowledge = true.
    fn make_h2_engine() -> HttpEngineSend<TokioRuntime, TcpConnector> {
        HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
            .http2_prior_knowledge()
            .build()
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
        // Trying to do TLS handshake on a non-TLS TCP stream should fail
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server sends garbage (not TLS) and closes
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
        // The TLS handshake should fail quickly since the server responds with non-TLS data
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            engine.connect_tls(stream, "example.com"),
        )
        .await
        .expect("tls handshake should complete within timeout");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connect_tunnel_success_200() {
        // Simulate a CONNECT proxy that responds with 200 OK then drops
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the CONNECT request
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Verify it's a CONNECT request
            let req_str = String::from_utf8_lossy(&request);
            assert!(
                req_str.starts_with("CONNECT "),
                "should be a CONNECT request"
            );
            assert!(
                req_str.contains("target.example.com:443"),
                "should target the correct host"
            );

            // Respond with 200
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();

            // Drop the connection immediately so TLS handshake fails with EOF
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        // connect_tunnel will succeed the CONNECT handshake but then try TLS
        // which will fail since no TLS connector is configured (make_engine() has no TLS)
        let result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority)
            .await;
        assert!(
            result.is_err(),
            "should fail because no TLS connector configured"
        );
    }

    #[tokio::test]
    async fn connect_tunnel_proxy_returns_403() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Respond with 403 Forbidden
            server_io
                .write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n")
                .await
                .unwrap();
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        let result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("CONNECT tunnel failed"),
            "error should mention tunnel failure, got: {err}"
        );
        assert!(
            err.contains("403"),
            "error should contain the status code, got: {err}"
        );
    }

    #[tokio::test]
    async fn connect_tunnel_proxy_closes_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncReadExt;
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the full CONNECT request first
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Drop without sending any response - client sees EOF during read
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        let result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("proxy closed connection"),
            "error should mention proxy closure, got: {err}"
        );
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

    #[tokio::test]
    async fn connect_tunnel_sends_proxy_auth_header() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the full CONNECT request
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            // Respond with 200
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            // Drop to trigger TLS failure
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080")
            .unwrap()
            .basic_auth("user", "password");
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        // connect_tunnel will succeed the CONNECT handshake, send auth header,
        // then TLS fails because no TLS connector configured
        let _result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority)
            .await;

        // Verify the captured request contains the Proxy-Authorization header
        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("Proxy-Authorization: Basic"),
            "CONNECT request should include Proxy-Authorization header, got: {captured}"
        );
        assert!(
            captured.contains("CONNECT target.example.com:443"),
            "CONNECT request should target the correct host, got: {captured}"
        );
    }

    #[tokio::test]
    async fn connect_tunnel_response_too_large() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            // Read the full CONNECT request
            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            // Send a huge response (> 8192 bytes) without ending with \r\n\r\n
            let big_chunk = vec![b'A'; 9000];
            server_io.write_all(&big_chunk).await.unwrap();
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "target.example.com:443".parse().unwrap();

        let result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("too large"),
            "error should mention response too large, got: {err}"
        );
    }
}
