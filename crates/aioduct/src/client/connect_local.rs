use std::future::Future;
use std::pin::Pin;

use crate::body::RequestBoxLocalBody;
use crate::error::Error;
use crate::pool::PooledConnection;
use crate::proxy::ProxyConfig;
use crate::runtime::{Connector, RuntimeLocal, SocketConfig};

use super::HttpEngineLocal;

impl<R: RuntimeLocal, C: Connector + Clone> HttpEngineLocal<R, C> {
    pub(super) async fn connect_via_proxy_local(
        &self,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
    ) -> Result<PooledConnection<RequestBoxLocalBody>, Error> {
        let proxy_authority = proxy.authority()?;
        let default_port = proxy.default_port();
        let proxy_addr = self
            .core
            .resolve_authority(proxy_authority, default_port)
            .await?;
        let mut tcp_stream = if let Some(local_addr) = self.core.local_address {
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
            crate::socks5::socks5_handshake(&mut tcp_stream, host, port, proxy.auth.as_ref())
                .await
                .map_err(Error::Io)?;
            if is_https {
                self.connect_tls_local(tcp_stream, host).await
            } else {
                self.connect_h1_local(tcp_stream).await
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
                self.connect_tls_local(tcp_stream, host).await
            } else {
                self.connect_h1_local(tcp_stream).await
            }
        } else if is_https {
            self.connect_tunnel_local(tcp_stream, proxy, target_authority)
                .await
        } else {
            self.connect_plaintext_local(tcp_stream).await
        }
    }

    async fn connect_tunnel_local(
        &self,
        mut tcp_stream: C::Stream,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
    ) -> Result<PooledConnection<RequestBoxLocalBody>, Error> {
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

        let status_code = super::connect::parse_connect_status(status_line)?;
        if status_code != 200 {
            return Err(Error::Other(
                format!("CONNECT tunnel failed: {status_line}").into(),
            ));
        }

        self.connect_tls_local(tcp_stream, target_authority.host())
            .await
    }

    pub(super) fn connect_plaintext_local<S>(
        &self,
        stream: S,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBoxLocalBody>, Error>> + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_plaintext_local_with_hint(stream, false)
    }

    pub(super) fn connect_plaintext_local_with_hint<S>(
        &self,
        stream: S,
        force_h2c: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBoxLocalBody>, Error>> + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        if self.core.http2_prior_knowledge || force_h2c {
            Box::pin(self.connect_h2_prior_knowledge_local(stream))
        } else {
            Box::pin(self.connect_h1_local(stream))
        }
    }

    pub(super) async fn connect_h1_local<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBoxLocalBody>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let (sender, conn) = hyper::client::conn::http1::handshake(stream).await?;
        R::spawn_local(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h1(sender))
    }

    pub(super) async fn connect_h2_prior_knowledge_local<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBoxLocalBody>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let mut builder = hyper::client::conn::http2::Builder::new(
            crate::runtime::executor::completion_executor::<R>(),
        );
        if let Some(ref h2) = self.core.http2 {
            h2.apply(&mut builder);
        }
        let (sender, conn) = builder.handshake(stream).await?;
        R::spawn_local(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h2(sender))
    }

    #[cfg(all(feature = "rustls", feature = "compio"))]
    pub(super) async fn connect_tls_local(
        &self,
        tcp_stream: C::Stream,
        host: &str,
    ) -> Result<PooledConnection<RequestBoxLocalBody>, Error> {
        use crate::tls::TlsConnectLocal;
        use std::time::Instant;

        let tls_start = Instant::now();

        let tls_connector = self
            .core
            .tls
            .as_ref()
            .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

        let tls_stream =
            <crate::tls::RustlsConnector as TlsConnectLocal<C::Stream>>::connect_local(
                tls_connector,
                host,
                tcp_stream,
            )
            .await
            .map_err(|e| Error::Tls(Box::new(e)))?;

        let tls_duration = tls_start.elapsed();

        let alpn = crate::tls::RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
        let tls_info = tls_stream.tls_info();

        match alpn {
            Some(crate::tls::AlpnProtocol::H2) => {
                let mut builder = hyper::client::conn::http2::Builder::new(
                    crate::runtime::executor::completion_executor::<R>(),
                );
                if let Some(ref h2) = self.core.http2 {
                    h2.apply(&mut builder);
                }
                let (sender, conn) = builder.handshake(tls_stream).await?;
                R::spawn_local(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h2(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn_local(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h1(sender);
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
        }
    }

    #[cfg(all(feature = "rustls", not(feature = "compio")))]
    pub(super) async fn connect_tls_local(
        &self,
        _tcp_stream: C::Stream,
        _host: &str,
    ) -> Result<PooledConnection<RequestBoxLocalBody>, Error> {
        Err(Error::Tls(
            "TLS with !Send streams requires the compio feature".into(),
        ))
    }

    #[cfg(not(feature = "rustls"))]
    pub(super) async fn connect_tls_local(
        &self,
        _tcp_stream: C::Stream,
        _host: &str,
    ) -> Result<PooledConnection<RequestBoxLocalBody>, Error> {
        Err(Error::Tls(
            "HTTPS requires the `rustls` TLS backend feature".into(),
        ))
    }
}
