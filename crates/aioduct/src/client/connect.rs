use std::future::Future;
use std::pin::Pin;

use http::Uri;

use crate::error::{AioductBody, Error};
use crate::pool::{HttpConnection, PooledConnection};
use crate::proxy::ProxyConfig;
use crate::response::Response;
use crate::runtime::Runtime;

use super::Client;

impl<R: Runtime> Client<R> {
    pub(super) async fn execute_single(
        &self,
        request: http::Request<AioductBody>,
        original_uri: &Uri,
    ) -> Result<Response, Error> {
        if let Some(ref limiter) = self.rate_limiter {
            while !limiter.try_acquire() {
                let wait = limiter.wait_duration();
                R::sleep(wait).await;
            }
        }

        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        let pool_key = crate::pool::PoolKey::new(scheme.clone(), authority.clone());

        if !self.no_connection_reuse {
            if let Some(mut conn) = self.pool.checkout(&pool_key) {
                #[cfg(feature = "tracing")]
                tracing::trace!(authority = %authority, "connection.pool.hit");

                let mut resp =
                    Self::send_on_connection(&mut conn, request, original_uri.clone()).await?;
                resp.set_remote_addr(conn.remote_addr);
                resp.set_tls_info(conn.tls_info.clone());
                if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                    self.pool.checkin(pool_key, conn);
                }
                return Ok(resp);
            }
        }

        #[cfg(feature = "http3")]
        if is_https {
            if let Some(endpoint) = &self.h3_endpoint {
                let use_h3 = self.prefer_h3 || self.alt_svc_cache.lookup_h3(authority).is_some();
                if use_h3 {
                    let default_port = 443u16;
                    let (h3_host, h3_port) = self
                        .alt_svc_cache
                        .lookup_h3(authority)
                        .unwrap_or_else(|| (None, authority.port_u16().unwrap_or(default_port)));
                    let connect_host = h3_host.as_deref().unwrap_or(authority.host());
                    let addr = self.resolve_authority_raw(connect_host, h3_port).await?;
                    let sni_host = authority.host().to_owned();
                    let quinn_conn = endpoint
                        .connect(addr, &sni_host)
                        .map_err(|e| Error::Other(Box::new(e)))?
                        .await
                        .map_err(|e| Error::Other(Box::new(e)))?;
                    let mut pooled = crate::h3_transport::connect_h3::<R>(quinn_conn).await?;
                    pooled.remote_addr = Some(addr);
                    let mut resp =
                        Self::send_on_connection(&mut pooled, request, original_uri.clone())
                            .await?;
                    resp.set_remote_addr(pooled.remote_addr);
                    resp.set_tls_info(pooled.tls_info.clone());
                    if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                        self.pool.checkin(pool_key, pooled);
                    }
                    return Ok(resp);
                }
            }
        }

        let proxy = self
            .proxy
            .as_ref()
            .and_then(|settings| settings.proxy_for(original_uri));

        #[cfg(unix)]
        let unix_socket = self.unix_socket.as_ref();
        #[cfg(not(unix))]
        let unix_socket: Option<&std::path::PathBuf> = None;

        let mut pooled = if let Some(unix_path) = unix_socket {
            let _ = &proxy; // suppress unused warning when unix_socket is set
            #[cfg(unix)]
            {
                let connect_fut = async {
                    let unix_stream = R::connect_unix(unix_path).await.map_err(Error::Io)?;
                    self.connect_plaintext(unix_stream).await
                };
                match self.connect_timeout {
                    Some(duration) => {
                        crate::timeout::Timeout::WithTimeout {
                            future: connect_fut,
                            sleep: R::sleep(duration),
                        }
                        .await?
                    }
                    None => connect_fut.await?,
                }
            }
            #[cfg(not(unix))]
            unreachable!()
        } else if let Some(ref proxy) = proxy {
            self.connect_via_proxy(proxy, authority, is_https).await?
        } else {
            let default_port = if is_https { 443 } else { 80 };
            let host = authority.host();
            let port = authority.port_u16().unwrap_or(default_port);
            let addrs = self.resolve_all_authority_raw(host, port).await?;

            let tcp_keepalive = self.tcp_keepalive;
            let tcp_keepalive_interval = self.tcp_keepalive_interval;
            let tcp_keepalive_retries = self.tcp_keepalive_retries;
            let tcp_fast_open = self.tcp_fast_open;
            let local_address = self.local_address;
            #[cfg(target_os = "linux")]
            let interface = self.interface.as_deref();
            let connect_fut = async {
                #[cfg(feature = "tracing")]
                tracing::trace!(addrs = ?addrs, "tcp.connect.start");

                let (tcp_stream, addr) = if addrs.len() > 1 && local_address.is_none() {
                    #[cfg(feature = "tower")]
                    let _ = original_uri;
                    crate::happy_eyeballs::connect_happy_eyeballs::<R>(&addrs, local_address)
                        .await
                        .map_err(Error::Io)?
                } else {
                    let addr = addrs[0];
                    let stream = if let Some(local_addr) = local_address {
                        R::connect_bound(addr, local_addr)
                            .await
                            .map_err(Error::Io)?
                    } else {
                        #[cfg(feature = "tower")]
                        if let Some(ref connector) = self.connector {
                            let info = crate::connector::ConnectInfo {
                                uri: original_uri.clone(),
                                addr,
                            };
                            connector.connect(info).await.map_err(Error::Io)?
                        } else {
                            R::connect(addr).await?
                        }
                        #[cfg(not(feature = "tower"))]
                        R::connect(addr).await?
                    };
                    (stream, addr)
                };

                #[cfg(target_os = "linux")]
                if let Some(iface) = interface {
                    R::bind_device(&tcp_stream, iface)?;
                }
                if let Some(time) = tcp_keepalive {
                    R::set_tcp_keepalive(
                        &tcp_stream,
                        time,
                        tcp_keepalive_interval,
                        tcp_keepalive_retries,
                    )?;
                }
                if tcp_fast_open {
                    let _ = R::set_tcp_fast_open(&tcp_stream);
                }
                #[cfg(feature = "tracing")]
                tracing::trace!(addr = %addr, "tcp.connect.done");

                let mut conn = if is_https {
                    self.connect_tls(tcp_stream, authority.host()).await?
                } else {
                    self.connect_plaintext(tcp_stream).await?
                };
                conn.remote_addr = Some(addr);
                Ok::<PooledConnection<R>, Error>(conn)
            };

            match self.connect_timeout {
                Some(duration) => {
                    crate::timeout::Timeout::WithTimeout {
                        future: connect_fut,
                        sleep: R::sleep(duration),
                    }
                    .await?
                }
                None => connect_fut.await?,
            }
        };

        let mut resp = Self::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
        resp.set_remote_addr(pooled.remote_addr);
        resp.set_tls_info(pooled.tls_info.clone());
        if !self.no_connection_reuse && resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            self.pool.checkin(pool_key, pooled);
        }

        Ok(resp)
    }

    async fn connect_via_proxy(
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

        #[cfg(feature = "tracing")]
        tracing::trace!(host = host, "tls.handshake.start");

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
                Ok(pooled)
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h1(sender);
                pooled.tls_info = Some(tls_info);
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
        Err(Error::Tls("HTTPS requires the `rustls` feature".into()))
    }

    async fn send_on_connection(
        conn: &mut PooledConnection<R>,
        request: http::Request<AioductBody>,
        url: Uri,
    ) -> Result<Response, Error> {
        #[cfg(feature = "tracing")]
        let proto = match &conn.conn {
            HttpConnection::H1(_) => "h1",
            HttpConnection::H2(_) => "h2",
            #[cfg(feature = "http3")]
            HttpConnection::H3(_) => "h3",
        };
        #[cfg(feature = "tracing")]
        tracing::trace!(protocol = proto, uri = %url, "http.send.start");

        let result = match &mut conn.conn {
            HttpConnection::H1(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBody::from_incoming);
                Ok(Response::new(resp, url))
            }
            HttpConnection::H2(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(crate::response::ResponseBody::from_incoming);
                Ok(Response::new(resp, url))
            }
            #[cfg(feature = "http3")]
            HttpConnection::H3(sender) => {
                crate::h3_transport::send_on_h3(sender, request, url).await
            }
        };

        #[cfg(feature = "tracing")]
        if let Ok(ref resp) = result {
            tracing::trace!(status = resp.status().as_u16(), "http.send.done");
        }

        result
    }

    async fn resolve_authority(
        &self,
        authority: &http::uri::Authority,
        default_port: u16,
    ) -> Result<std::net::SocketAddr, Error> {
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(default_port);
        self.resolve_authority_raw(host, port).await
    }

    async fn resolve_authority_raw(
        &self,
        host: &str,
        port: u16,
    ) -> Result<std::net::SocketAddr, Error> {
        self.resolve_all_authority_raw(host, port)
            .await
            .map(|addrs| addrs[0])
    }

    async fn resolve_all_authority_raw(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, Error> {
        if let Ok(addr) = format!("{host}:{port}").parse::<std::net::SocketAddr>() {
            return Ok(vec![addr]);
        }

        #[cfg(feature = "tracing")]
        tracing::trace!(host = host, port = port, "dns.resolve.start");

        let result = if let Some(resolver) = &self.resolver {
            resolver
                .resolve_all(host, port)
                .await
                .map_err(|e| Error::InvalidUrl(format!("cannot resolve {host}:{port}: {e}")))
        } else {
            R::resolve_all(host, port)
                .await
                .map_err(|e| Error::InvalidUrl(format!("cannot resolve {host}:{port}: {e}")))
        };

        #[cfg(feature = "tracing")]
        match &result {
            Ok(addrs) => tracing::trace!(host = host, count = addrs.len(), "dns.resolve.done"),
            Err(e) => tracing::trace!(host = host, error = %e, "dns.resolve.error"),
        }

        result
    }

    #[cfg(feature = "http3")]
    pub(super) fn cache_alt_svc(&self, uri: &Uri, headers: &http::HeaderMap) {
        use http::header::ALT_SVC;
        if let Some(authority) = uri.authority() {
            if let Some(alt_svc_value) = headers.get(ALT_SVC) {
                if let Ok(value_str) = alt_svc_value.to_str() {
                    let entries = crate::alt_svc::parse_alt_svc(value_str);
                    self.alt_svc_cache.insert(authority.clone(), entries);
                }
            }
        }
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
