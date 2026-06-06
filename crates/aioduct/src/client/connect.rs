use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

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
        connect_timeout: Option<Duration>,
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

        if proxy.scheme == crate::proxy::ProxyScheme::Socks5
            || proxy.scheme == crate::proxy::ProxyScheme::Socks5h
        {
            let host = target_authority.host();
            let port = target_authority
                .port_u16()
                .unwrap_or(if is_https { 443 } else { 80 });
            let dns = if proxy.scheme == crate::proxy::ProxyScheme::Socks5h {
                crate::socks5::Socks5Dns::Remote
            } else {
                crate::socks5::Socks5Dns::Local
            };
            // Pre-resolve for Socks5Dns::Local
            let resolved_addr = if dns == crate::socks5::Socks5Dns::Local {
                let addr = self.core.resolve_authority(target_authority, port).await?;
                Some(addr.ip())
            } else {
                None
            };
            let mut stream = tcp_stream;
            crate::timeout::connect_timeout::<R, _, _>(
                async {
                    crate::socks5::socks5_handshake_async(
                        &mut stream,
                        host,
                        port,
                        proxy.auth.as_ref(),
                        dns,
                        resolved_addr,
                    )
                    .await
                    .map_err(Error::Io)
                },
                connect_timeout,
            )
            .await?;
            if is_https {
                self.connect_tls(stream, host).await
            } else if self.core.http2_prior_knowledge {
                self.connect_h2_prior_knowledge(stream).await
            } else {
                self.connect_h1(stream).await
            }
        } else if proxy.scheme == crate::proxy::ProxyScheme::Socks4 {
            let host = target_authority.host();
            let port = target_authority
                .port_u16()
                .unwrap_or(if is_https { 443 } else { 80 });
            let mut std_stream = self.connector.into_std_tcp(tcp_stream).map_err(Error::Io)?;
            if let Some(timeout) = connect_timeout {
                std_stream
                    .set_read_timeout(Some(timeout))
                    .map_err(Error::Io)?;
                std_stream
                    .set_write_timeout(Some(timeout))
                    .map_err(Error::Io)?;
            }
            crate::socks4::socks4a_handshake(&mut std_stream, host, port, proxy.auth.as_ref())
                .map_err(Error::Io)?;
            if connect_timeout.is_some() {
                std_stream.set_read_timeout(None).map_err(Error::Io)?;
                std_stream.set_write_timeout(None).map_err(Error::Io)?;
            }
            let tcp_stream = self.connector.from_std_tcp(std_stream).map_err(Error::Io)?;
            if is_https {
                self.connect_tls(tcp_stream, host).await
            } else if self.core.http2_prior_knowledge {
                self.connect_h2_prior_knowledge(tcp_stream).await
            } else {
                self.connect_h1(tcp_stream).await
            }
        } else if proxy.scheme == crate::proxy::ProxyScheme::Https {
            #[cfg(feature = "rustls")]
            {
                use crate::tls::TlsConnect;
                let tls_connector = self
                    .core
                    .tls
                    .as_ref()
                    .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;
                let tls_stream = <crate::tls::RustlsConnector as TlsConnect<C::Stream>>::connect(
                    tls_connector,
                    proxy_authority.host(),
                    tcp_stream,
                )
                .await
                .map_err(|e| Error::Tls(Box::new(e)))?;
                if is_https {
                    self.connect_tunnel(tls_stream, proxy, target_authority, connect_timeout)
                        .await
                } else {
                    // HTTPS proxy for HTTP target: CONNECT through TLS pipe.
                    let port = target_authority.port_u16().unwrap_or(80);
                    let target = format!("{}:{port}", target_authority.host());
                    let tunnel_stream =
                        super::connect_handshake::do_connect_handshake(tls_stream, proxy, &target)
                            .await?;
                    if self.core.http2_prior_knowledge {
                        self.connect_h2_prior_knowledge(tunnel_stream).await
                    } else {
                        self.connect_h1(tunnel_stream).await
                    }
                }
            }
            #[cfg(not(feature = "rustls"))]
            {
                Err(Error::Tls(
                    "HTTPS proxy requires the `rustls` TLS backend feature".into(),
                ))
            }
        } else if is_https {
            self.connect_tunnel(tcp_stream, proxy, target_authority, connect_timeout)
                .await
        } else {
            // HTTP proxy for HTTP target: CONNECT to create a raw pipe.
            let port = target_authority.port_u16().unwrap_or(80);
            let target = format!("{}:{port}", target_authority.host());
            let tunnel_stream =
                super::connect_handshake::do_connect_handshake(tcp_stream, proxy, &target).await?;
            if self.core.http2_prior_knowledge {
                self.connect_h2_prior_knowledge(tunnel_stream).await
            } else {
                self.connect_h1(tunnel_stream).await
            }
        }
    }

    /// Perform an HTTP CONNECT handshake through `stream` to `target`.
    /// Returns the stream unchanged on success (type-preserving for chaining).
    async fn connect_tunnel<S>(
        &self,
        stream: S,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
        _connect_timeout: Option<Duration>,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let port = target_authority.port_u16().unwrap_or(443);
        let target = format!("{}:{port}", target_authority.host());
        let tunnel_stream =
            super::connect_handshake::do_connect_handshake(stream, proxy, &target).await?;
        #[cfg(feature = "rustls")]
        {
            let stream = tunnel_stream;
            let host = target_authority.host();
            use crate::tls::TlsConnect;
            use std::time::Instant;

            let tls_connector = self
                .core
                .tls
                .as_ref()
                .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

            let tls_start = Instant::now();
            let tls_stream = <crate::tls::RustlsConnector as TlsConnect<S>>::connect(
                tls_connector,
                host,
                stream,
            )
            .await
            .map_err(|e| {
                #[cfg(feature = "tracing")]
                tracing::trace!(host = host, error = %e, "tls.handshake.error");
                Error::Tls(Box::new(e))
            })?;

            let tls_duration = tls_start.elapsed();
            let alpn =
                crate::tls::RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
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
        {
            drop(tunnel_stream);
            Err(Error::Tls(
                "HTTPS CONNECT tunnel requires the `rustls` TLS backend feature".into(),
            ))
        }
    }

    async fn connect_two_hop_send(
        &self,
        first: &ProxyConfig,
        second: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
        connect_timeout: Option<Duration>,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        let second_authority = second.authority()?;
        let second_default_port = second.default_port();
        let second_host = second_authority.host();
        let second_port = second_authority.port_u16().unwrap_or(second_default_port);

        let first_authority = first.authority()?;
        let first_addr = self
            .core
            .resolve_authority(first_authority, first.default_port())
            .await?;

        let tcp_stream = if let Some(local_addr) = self.core.local_address {
            self.connector
                .connect_bound(first_addr, local_addr)
                .await
                .map_err(Error::Io)?
        } else {
            self.connector
                .connect(first_addr)
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

        if first.scheme == crate::proxy::ProxyScheme::Socks5
            || first.scheme == crate::proxy::ProxyScheme::Socks5h
        {
            let dns = if first.scheme == crate::proxy::ProxyScheme::Socks5h {
                crate::socks5::Socks5Dns::Remote
            } else {
                crate::socks5::Socks5Dns::Local
            };
            let resolved_addr = if dns == crate::socks5::Socks5Dns::Local {
                let addr = self
                    .core
                    .resolve_authority(second_authority, second_default_port)
                    .await?;
                Some(addr.ip())
            } else {
                None
            };
            let mut stream = tcp_stream;
            crate::timeout::connect_timeout::<R, _, _>(
                async {
                    crate::socks5::socks5_handshake_async(
                        &mut stream,
                        second_host,
                        second_port,
                        first.auth.as_ref(),
                        dns,
                        resolved_addr,
                    )
                    .await
                    .map_err(Error::Io)
                },
                connect_timeout,
            )
            .await?;
            self.connect_second_hop_send(
                stream,
                second,
                target_authority,
                is_https,
                connect_timeout,
            )
            .await
        } else if first.scheme == crate::proxy::ProxyScheme::Socks4 {
            let mut std_stream = self.connector.into_std_tcp(tcp_stream).map_err(Error::Io)?;
            if let Some(timeout) = connect_timeout {
                std_stream
                    .set_read_timeout(Some(timeout))
                    .map_err(Error::Io)?;
                std_stream
                    .set_write_timeout(Some(timeout))
                    .map_err(Error::Io)?;
            }
            crate::socks4::socks4a_handshake(
                &mut std_stream,
                second_host,
                second_port,
                first.auth.as_ref(),
            )
            .map_err(Error::Io)?;
            if connect_timeout.is_some() {
                std_stream.set_read_timeout(None).map_err(Error::Io)?;
                std_stream.set_write_timeout(None).map_err(Error::Io)?;
            }
            let stream = self.connector.from_std_tcp(std_stream).map_err(Error::Io)?;
            self.connect_second_hop_send(
                stream,
                second,
                target_authority,
                is_https,
                connect_timeout,
            )
            .await
        } else if first.scheme == crate::proxy::ProxyScheme::Https {
            #[cfg(feature = "rustls")]
            {
                use crate::tls::TlsConnect;
                let tls_connector = self
                    .core
                    .tls
                    .as_ref()
                    .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;
                let tls_stream = <crate::tls::RustlsConnector as TlsConnect<C::Stream>>::connect(
                    tls_connector,
                    first_authority.host(),
                    tcp_stream,
                )
                .await
                .map_err(|e| Error::Tls(Box::new(e)))?;
                let second_target = format!(
                    "{}:{}",
                    second_authority.host(),
                    second_authority.port_u16().unwrap_or(second.default_port())
                );
                let stream = super::connect_handshake::do_connect_handshake(
                    tls_stream,
                    first,
                    &second_target,
                )
                .await?;
                if is_https {
                    self.connect_tunnel(stream, second, target_authority, connect_timeout)
                        .await
                } else {
                    self.connect_plaintext(stream).await
                }
            }
            #[cfg(not(feature = "rustls"))]
            {
                Err(Error::Tls(
                    "HTTPS proxy requires the `rustls` TLS backend feature".into(),
                ))
            }
        } else {
            // HTTP proxy: CONNECT through first to reach second
            let second_target = format!(
                "{}:{}",
                second_authority.host(),
                second_authority.port_u16().unwrap_or(second.default_port())
            );
            let stream =
                super::connect_handshake::do_connect_handshake(tcp_stream, first, &second_target)
                    .await?;
            self.connect_second_hop_send(
                stream,
                second,
                target_authority,
                is_https,
                connect_timeout,
            )
            .await
        }
    }

    /// Second-hop dispatch: the stream is already connected to `second`'s proxy.
    /// Routes via SOCKS5/SOCKS4 handshake or HTTP CONNECT depending on the scheme.
    async fn connect_second_hop_send(
        &self,
        stream: C::Stream,
        second: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
        connect_timeout: Option<Duration>,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        let target_host = target_authority.host();
        let target_port = target_authority
            .port_u16()
            .unwrap_or(if is_https { 443 } else { 80 });

        if second.scheme == crate::proxy::ProxyScheme::Socks5
            || second.scheme == crate::proxy::ProxyScheme::Socks5h
        {
            let dns = if second.scheme == crate::proxy::ProxyScheme::Socks5h {
                crate::socks5::Socks5Dns::Remote
            } else {
                crate::socks5::Socks5Dns::Local
            };
            let resolved_addr = if dns == crate::socks5::Socks5Dns::Local {
                let addr = self
                    .core
                    .resolve_authority(target_authority, target_port)
                    .await?;
                Some(addr.ip())
            } else {
                None
            };
            let mut s = stream;
            crate::timeout::connect_timeout::<R, _, _>(
                async {
                    crate::socks5::socks5_handshake_async(
                        &mut s,
                        target_host,
                        target_port,
                        second.auth.as_ref(),
                        dns,
                        resolved_addr,
                    )
                    .await
                    .map_err(Error::Io)
                },
                connect_timeout,
            )
            .await?;
            if is_https {
                self.connect_tls(s, target_host).await
            } else if self.core.http2_prior_knowledge {
                self.connect_h2_prior_knowledge(s).await
            } else {
                self.connect_h1(s).await
            }
        } else if second.scheme == crate::proxy::ProxyScheme::Socks4 {
            let mut std_stream = self.connector.into_std_tcp(stream).map_err(Error::Io)?;
            if let Some(timeout) = connect_timeout {
                std_stream
                    .set_read_timeout(Some(timeout))
                    .map_err(Error::Io)?;
                std_stream
                    .set_write_timeout(Some(timeout))
                    .map_err(Error::Io)?;
            }
            crate::socks4::socks4a_handshake(
                &mut std_stream,
                target_host,
                target_port,
                second.auth.as_ref(),
            )
            .map_err(Error::Io)?;
            if connect_timeout.is_some() {
                std_stream.set_read_timeout(None).map_err(Error::Io)?;
                std_stream.set_write_timeout(None).map_err(Error::Io)?;
            }
            let stream = self.connector.from_std_tcp(std_stream).map_err(Error::Io)?;
            if is_https {
                self.connect_tls(stream, target_host).await
            } else {
                self.connect_plaintext(stream).await
            }
        } else if second.scheme == crate::proxy::ProxyScheme::Https {
            #[cfg(feature = "rustls")]
            {
                use crate::tls::TlsConnect;
                let tls_connector = self
                    .core
                    .tls
                    .as_ref()
                    .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;
                let second_authority = second.authority()?;
                let tls_stream = <crate::tls::RustlsConnector as TlsConnect<C::Stream>>::connect(
                    tls_connector,
                    second_authority.host(),
                    stream,
                )
                .await
                .map_err(|e| Error::Tls(Box::new(e)))?;
                if is_https {
                    self.connect_tunnel(tls_stream, second, target_authority, connect_timeout)
                        .await
                } else {
                    self.connect_plaintext(tls_stream).await
                }
            }
            #[cfg(not(feature = "rustls"))]
            {
                Err(Error::Tls(
                    "HTTPS proxy requires the `rustls` TLS backend feature".into(),
                ))
            }
        } else {
            // HTTP: CONNECT through second to reach target
            if is_https {
                self.connect_tunnel(stream, second, target_authority, connect_timeout)
                    .await
            } else {
                self.connect_plaintext(stream).await
            }
        }
    }

    pub(super) async fn connect_via_proxy_chain(
        &self,
        chain: &crate::proxy::ProxyChain,
        target_authority: &http::uri::Authority,
        is_https: bool,
        connect_timeout: Option<Duration>,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        match chain.len() {
            0 => Err(Error::Other("empty proxy chain".into())),
            1 => {
                self.connect_via_proxy(
                    &chain.proxies[0],
                    target_authority,
                    is_https,
                    connect_timeout,
                )
                .await
            }
            2 => {
                self.connect_two_hop_send(
                    &chain.proxies[0],
                    &chain.proxies[1],
                    target_authority,
                    is_https,
                    connect_timeout,
                )
                .await
            }
            n => Err(Error::Other(
                format!("proxy chains longer than 2 hops are not yet supported (got {n})").into(),
            )),
        }
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

    #[cfg(feature = "rustls")]
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
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
            .await;
        assert!(
            result.is_err(),
            "should fail because no TLS connector configured"
        );
    }

    #[cfg(not(feature = "rustls"))]
    #[tokio::test]
    async fn connect_tunnel_requires_rustls_feature() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let (mut server_io, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();

            loop {
                let n = server_io.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }

            let req_str = String::from_utf8_lossy(&request);
            assert!(req_str.starts_with("CONNECT target.example.com:443 "));
            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
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
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
            .await;
        match result {
            Err(crate::Error::Tls(err)) => {
                assert!(
                    err.to_string()
                        .contains("requires the `rustls` TLS backend feature")
                );
            }
            Ok(_) => panic!("CONNECT tunnel unexpectedly succeeded without rustls"),
            Err(err) => panic!("expected TLS feature error, got {err}"),
        }
        proxy_task.await.unwrap();
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_defaults_port_443_when_authority_has_no_port() {
        // When the URL has no explicit port (e.g. https://example.com/),
        // the authority is "example.com" without ":443".
        // connect_tunnel must add the port so CONNECT targets the right port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

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

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        // Authority WITHOUT explicit port — connect_tunnel must add :443
        let target_authority: http::uri::Authority = "target.example.com".parse().unwrap();

        // TLS will fail (no TLS connector in make_engine), but the CONNECT
        // handshake should succeed and the capture should show the target
        // includes :443.
        let _result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
            .await;

        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("CONNECT target.example.com:443"),
            "CONNECT target must include port 443 when authority lacks explicit port, got: {captured}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_defaults_port_443_for_ipv6_without_port() {
        // IPv6 authorities like "[::1]" must still get ":443" appended.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

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

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "[::1]".parse().unwrap();

        let _result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
            .await;

        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("CONNECT [::1]:443"),
            "IPv6 CONNECT target must include port 443, got: {captured}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_tunnel_preserves_explicit_port() {
        // When the authority already has an explicit port, it must be kept.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let (captured_tx, mut captured_rx) = tokio::sync::oneshot::channel::<String>();

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

            let req_str = String::from_utf8_lossy(&request).to_string();
            let _ = captured_tx.send(req_str);

            server_io
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            drop(server_io);
        });

        let connector = TcpConnector;
        let tcp_stream =
            <TcpConnector as crate::runtime::ConnectorSend>::connect(&connector, proxy_addr)
                .await
                .unwrap();
        let engine = make_engine();
        let proxy = crate::proxy::ProxyConfig::http("http://proxy.example.com:8080").unwrap();
        let target_authority: http::uri::Authority = "example.com:8443".parse().unwrap();

        let _result = engine
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
            .await;

        let captured = captured_rx.try_recv().unwrap();
        assert!(
            captured.contains("CONNECT example.com:8443"),
            "CONNECT target must preserve explicit port 8443, got: {captured}"
        );
    }

    #[cfg(feature = "rustls")]
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
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
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

    #[cfg(feature = "rustls")]
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
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
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

    #[cfg(feature = "rustls")]
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
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
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

    #[cfg(feature = "rustls")]
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
            .connect_tunnel(tcp_stream, &proxy, &target_authority, None)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("too large"),
            "error should mention response too large, got: {err}"
        );
    }

    // --- connect_via_proxy_chain tests ---

    #[tokio::test]
    async fn connect_via_proxy_chain_empty_is_error() {
        let engine = make_engine();
        let chain = crate::proxy::ProxyChain::new(vec![]);
        let authority: http::uri::Authority = "example.com:443".parse().unwrap();
        let result = engine
            .connect_via_proxy_chain(&chain, &authority, true, None)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("empty"),
            "expected empty chain error, got: {err}"
        );
    }

    #[tokio::test]
    async fn connect_via_proxy_chain_three_hops_is_error() {
        let engine = make_engine();
        let p1 = crate::proxy::ProxyConfig::http("http://p1:8080").unwrap();
        let p2 = crate::proxy::ProxyConfig::socks5("socks5://p2:1080").unwrap();
        let p3 = crate::proxy::ProxyConfig::http("http://p3:3128").unwrap();
        let chain = crate::proxy::ProxyChain::new(vec![p1, p2, p3]);
        let authority: http::uri::Authority = "example.com:443".parse().unwrap();
        let result = engine
            .connect_via_proxy_chain(&chain, &authority, true, None)
            .await;
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            err.contains("longer than 2 hops"),
            "expected chain length error, got: {err}"
        );
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn connect_two_hop_send_http_http_chain() {
        // Two-hop HTTP CONNECT chain:
        //   client → proxy1 (CONNECT to proxy2) → proxy2 (CONNECT to target)
        //
        // proxy2 returns 200 then stays open (no real TLS server behind it).
        // The client will attempt TLS to the target after both CONNECT
        // handshakes, and that TLS will fail. The key assertion is that the
        // error is NOT "CONNECT tunnel failed" — both tunnels opened.

        // proxy2: responds 200 to CONNECT the-target, then waits
        let proxy2_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy2_addr = proxy2_listener.local_addr().unwrap();

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = proxy2_listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains("CONNECT example.com:443"),
                "proxy2 should see CONNECT to target, got: {req}"
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        });

        // proxy1: reads CONNECT to proxy2, connects to proxy2, responds 200,
        // then relays all traffic bidirectionally
        let proxy1_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy1_addr = proxy1_listener.local_addr().unwrap();
        let proxy2_relay = proxy2_addr;

        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut client, _) = proxy1_listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = client.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.starts_with("CONNECT"),
                "proxy1 should see CONNECT, got: {req}"
            );

            // Connect to proxy2 to establish the tunnel
            let mut upstream = match tokio::net::TcpStream::connect(proxy2_relay).await {
                Ok(s) => s,
                Err(_) => {
                    let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                    return;
                }
            };
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();

            // Relay client ↔ proxy2
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });

        let engine = make_engine();
        let proxy1 = crate::proxy::ProxyConfig::http(&format!("http://{proxy1_addr}")).unwrap();
        let proxy2 = crate::proxy::ProxyConfig::http(&format!("http://{proxy2_addr}")).unwrap();
        let chain = crate::proxy::ProxyChain::new(vec![proxy1, proxy2]);
        let authority: http::uri::Authority = "example.com:443".parse().unwrap();

        let result = engine
            .connect_via_proxy_chain(&chain, &authority, true, None)
            .await;
        // Should open both tunnels then fail at TLS to the target
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(
            !err.contains("CONNECT tunnel failed"),
            "both tunnels should succeed, error should be TLS-related, got: {err}"
        );
    }
}
