use std::time::Duration;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::pool::PooledConnection;
use crate::proxy::ProxyConfig;
use crate::runtime::{ConnectorSend, RuntimePoll, SocketConfig};

use super::HttpEngineSend;

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    pub(crate) async fn connect_via_proxy_send(
        &self,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
        connect_timeout: Option<Duration>,
        force_h2c: bool,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        proxy.validate_for_use()?;
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
            } else if force_h2c {
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
            } else if force_h2c {
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
                    self.connect_tunnel_send(tls_stream, proxy, target_authority, connect_timeout)
                        .await
                } else {
                    // HTTPS proxy for HTTP target: CONNECT through TLS pipe.
                    let port = target_authority.port_u16().unwrap_or(80);
                    let target = format!("{}:{port}", target_authority.host());
                    let tunnel_stream =
                        super::connect_handshake::do_connect_handshake(tls_stream, proxy, &target)
                            .await?;
                    if force_h2c {
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
            self.connect_tunnel_send(tcp_stream, proxy, target_authority, connect_timeout)
                .await
        } else {
            // HTTP proxy for HTTP target: CONNECT to create a raw pipe.
            let port = target_authority.port_u16().unwrap_or(80);
            let target = format!("{}:{port}", target_authority.host());
            let tunnel_stream =
                super::connect_handshake::do_connect_handshake(tcp_stream, proxy, &target).await?;
            if force_h2c {
                self.connect_h2_prior_knowledge(tunnel_stream).await
            } else {
                self.connect_h1(tunnel_stream).await
            }
        }
    }

    /// Perform an HTTP CONNECT handshake through `stream` to `target`.
    /// Returns the stream unchanged on success (type-preserving for chaining).
    pub(crate) async fn connect_tunnel_send<S>(
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
        force_h2c: bool,
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
                force_h2c,
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
                force_h2c,
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
                if second.scheme == crate::proxy::ProxyScheme::Https {
                    // TLS-wrap to the second proxy through the first proxy's
                    // tunnel, then dispatch based on target scheme and h2c flag.
                    let tls_stream = <crate::tls::RustlsConnector as TlsConnect<_>>::connect(
                        tls_connector,
                        second_authority.host(),
                        stream,
                    )
                    .await
                    .map_err(|e| Error::Tls(Box::new(e)))?;
                    if is_https {
                        self.connect_tunnel_send(
                            tls_stream,
                            second,
                            target_authority,
                            connect_timeout,
                        )
                        .await
                    } else if force_h2c {
                        let port = target_authority.port_u16().unwrap_or(80);
                        let target = format!("{}:{port}", target_authority.host());
                        let tunnel_stream = super::connect_handshake::do_connect_handshake(
                            tls_stream, second, &target,
                        )
                        .await?;
                        self.connect_h2_prior_knowledge(tunnel_stream).await
                    } else {
                        self.connect_h1(tls_stream).await
                    }
                } else {
                    if is_https {
                        self.connect_tunnel_send(stream, second, target_authority, connect_timeout)
                            .await
                    } else if force_h2c {
                        let port = target_authority.port_u16().unwrap_or(80);
                        let target = format!("{}:{port}", target_authority.host());
                        let tunnel_stream =
                            super::connect_handshake::do_connect_handshake(stream, second, &target)
                                .await?;
                        self.connect_h2_prior_knowledge(tunnel_stream).await
                    } else {
                        self.connect_h1(stream).await
                    }
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
                force_h2c,
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
        force_h2c: bool,
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
            } else if force_h2c {
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
                self.connect_plaintext_with_hint(stream, force_h2c).await
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
                    self.connect_tunnel_send(tls_stream, second, target_authority, connect_timeout)
                        .await
                } else if force_h2c {
                    // H2 prior-knowledge through an HTTPS proxy: CONNECT first
                    // to create a raw pipe to the target, then send the preface.
                    let port = target_authority.port_u16().unwrap_or(80);
                    let target = format!("{}:{port}", target_authority.host());
                    let tunnel_stream =
                        super::connect_handshake::do_connect_handshake(tls_stream, second, &target)
                            .await?;
                    self.connect_h2_prior_knowledge(tunnel_stream).await
                } else {
                    self.connect_plaintext_with_hint(tls_stream, force_h2c)
                        .await
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
                self.connect_tunnel_send(stream, second, target_authority, connect_timeout)
                    .await
            } else if force_h2c {
                // H2 prior-knowledge through an HTTP proxy: CONNECT first
                // to create a raw pipe to the target, then send the preface.
                let port = target_authority.port_u16().unwrap_or(80);
                let target = format!("{}:{port}", target_authority.host());
                let tunnel_stream =
                    super::connect_handshake::do_connect_handshake(stream, second, &target).await?;
                self.connect_h2_prior_knowledge(tunnel_stream).await
            } else {
                self.connect_plaintext_with_hint(stream, force_h2c).await
            }
        }
    }

    pub(crate) async fn connect_via_proxy_chain_send(
        &self,
        chain: &crate::proxy::ProxyChain,
        target_authority: &http::uri::Authority,
        is_https: bool,
        connect_timeout: Option<Duration>,
        force_h2c: bool,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        for proxy in chain.iter() {
            proxy.validate_for_use()?;
        }
        match chain.len() {
            0 => Err(Error::Other("empty proxy chain".into())),
            1 => {
                self.connect_via_proxy_send(
                    &chain.proxies[0],
                    target_authority,
                    is_https,
                    connect_timeout,
                    force_h2c,
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
                    force_h2c,
                )
                .await
            }
            n => Err(Error::Other(
                format!("proxy chains longer than 2 hops are not yet supported (got {n})").into(),
            )),
        }
    }
}
