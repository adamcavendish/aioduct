use std::future::Future;
use std::pin::Pin;

use crate::error::Error;
use crate::pool::PooledConnection;
use crate::proxy::ProxyConfig;
use crate::runtime::Runtime;

use super::Client;

impl<R: Runtime> Client<R> {
    pub(super) async fn connect_via_proxy(
        &self,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
    ) -> Result<PooledConnection<R>, Error> {
        let proxy_authority = proxy.authority()?;
        let default_port = proxy.default_port();
        let proxy_addr = self
            .resolve_authority(proxy_authority, default_port)
            .await?;
        let mut tcp_stream = if let Some(local_addr) = self.local_address {
            R::connect_bound(proxy_addr, local_addr)
                .await
                .map_err(Error::Io)?
        } else {
            R::connect(proxy_addr).await?
        };
        #[cfg(target_os = "linux")]
        if let Some(ref iface) = self.interface {
            R::bind_device(&tcp_stream, iface)?;
        }
        if let Some(time) = self.tcp_keepalive {
            R::set_tcp_keepalive(
                &tcp_stream,
                time,
                self.tcp_keepalive_interval,
                self.tcp_keepalive_retries,
            )?;
        }
        if self.tcp_fast_open {
            let _ = R::set_tcp_fast_open(&tcp_stream);
        }

        if proxy.scheme == crate::proxy::ProxyScheme::Socks5 {
            let host = target_authority.host();
            let port = target_authority
                .port_u16()
                .unwrap_or(if is_https { 443 } else { 80 });
            crate::socks5::socks5_handshake(&mut tcp_stream, host, port, proxy.auth.as_ref())
                .await
                .map_err(Error::Io)?;
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
            crate::socks4::socks4a_handshake(&mut tcp_stream, host, port, proxy.auth.as_ref())
                .await
                .map_err(Error::Io)?;
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
        mut tcp_stream: R::TcpStream,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
    ) -> Result<PooledConnection<R>, Error> {
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
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<R>, Error>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        if self.http2_prior_knowledge {
            Box::pin(self.connect_h2_prior_knowledge(stream))
        } else {
            Box::pin(self.connect_h1(stream))
        }
    }

    async fn connect_h1<S>(&self, stream: S) -> Result<PooledConnection<R>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let (sender, conn) = hyper::client::conn::http1::handshake(stream).await?;
        R::spawn(async move {
            let _ = conn.with_upgrades().await;
        });
        Ok(PooledConnection::new_h1(sender))
    }

    async fn connect_h2_prior_knowledge<S>(&self, stream: S) -> Result<PooledConnection<R>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let mut builder =
            hyper::client::conn::http2::Builder::new(crate::runtime::hyper_executor::<R>());
        if let Some(ref h2) = self.http2 {
            if let Some(v) = h2.initial_stream_window_size {
                builder.initial_stream_window_size(v);
            }
            if let Some(v) = h2.initial_connection_window_size {
                builder.initial_connection_window_size(v);
            }
            if let Some(v) = h2.max_frame_size {
                builder.max_frame_size(v);
            }
            if let Some(v) = h2.adaptive_window {
                builder.adaptive_window(v);
            }
            if let Some(v) = h2.keep_alive_interval {
                builder.keep_alive_interval(v);
            }
            if let Some(v) = h2.keep_alive_timeout {
                builder.keep_alive_timeout(v);
            }
            if let Some(v) = h2.keep_alive_while_idle {
                builder.keep_alive_while_idle(v);
            }
            if let Some(v) = h2.max_header_list_size {
                builder.max_header_list_size(v);
            }
            if let Some(v) = h2.max_send_buf_size {
                builder.max_send_buf_size(v);
            }
            if let Some(v) = h2.max_concurrent_reset_streams {
                builder.max_concurrent_reset_streams(v);
            }
        }
        let (sender, conn) = builder.handshake(stream).await?;
        R::spawn(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h2(sender))
    }

    #[cfg(feature = "rustls")]
    pub(super) async fn connect_tls(
        &self,
        tcp_stream: R::TcpStream,
        host: &str,
    ) -> Result<PooledConnection<R>, Error> {
        use crate::tls::TlsConnect;
        use std::time::Instant;

        #[cfg(feature = "tracing")]
        tracing::trace!(host = host, "tls.handshake.start");

        let tls_start = Instant::now();

        let tls_connector = self
            .tls
            .as_ref()
            .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

        let tls_stream = <crate::tls::RustlsConnector as TlsConnect<R>>::connect(
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
                let mut builder =
                    hyper::client::conn::http2::Builder::new(crate::runtime::hyper_executor::<R>());
                if let Some(ref h2) = self.http2 {
                    if let Some(v) = h2.initial_stream_window_size {
                        builder.initial_stream_window_size(v);
                    }
                    if let Some(v) = h2.initial_connection_window_size {
                        builder.initial_connection_window_size(v);
                    }
                    if let Some(v) = h2.max_frame_size {
                        builder.max_frame_size(v);
                    }
                    if let Some(v) = h2.adaptive_window {
                        builder.adaptive_window(v);
                    }
                    if let Some(v) = h2.keep_alive_interval {
                        builder.keep_alive_interval(v);
                    }
                    if let Some(v) = h2.keep_alive_timeout {
                        builder.keep_alive_timeout(v);
                    }
                    if let Some(v) = h2.keep_alive_while_idle {
                        builder.keep_alive_while_idle(v);
                    }
                    if let Some(v) = h2.max_header_list_size {
                        builder.max_header_list_size(v);
                    }
                    if let Some(v) = h2.max_send_buf_size {
                        builder.max_send_buf_size(v);
                    }
                    if let Some(v) = h2.max_concurrent_reset_streams {
                        builder.max_concurrent_reset_streams(v);
                    }
                }
                let (sender, conn) = builder.handshake(tls_stream).await?;
                R::spawn(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h2(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn(async move {
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
        _tcp_stream: R::TcpStream,
        _host: &str,
    ) -> Result<PooledConnection<R>, Error> {
        Err(Error::Tls(
            "HTTPS requires the `rustls` TLS backend feature".into(),
        ))
    }
}

fn parse_connect_status(status_line: &str) -> Result<u16, Error> {
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
}
