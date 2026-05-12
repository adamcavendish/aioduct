use crate::clock::Instant;

use bytes::Bytes;
use http::Uri;
use http_body_util::BodyExt;

use crate::error::{AioductBody, Error};
use crate::observer::{self, RequestEvent, RequestPhase};
use crate::pool::{HttpConnection, PooledConnection, ProtocolHint};
use crate::response::{BodyObserverCtx, Response};
use crate::runtime::{ConnectorSend, RuntimePoll, SocketConfig};
#[allow(deprecated)]
use crate::timing::TimingCollector;

use super::HttpEngine;

impl<R: RuntimePoll, C: ConnectorSend> HttpEngine<R, C> {
    /// Populate SANs on a connection before returning it to the pool.
    #[cfg(feature = "rustls")]
    fn populate_sans(conn: &mut PooledConnection) {
        if conn.is_h2_or_h3()
            && conn.sans.is_empty()
            && let Some(der) = conn.tls_info.as_ref().and_then(|t| t.peer_certificate())
        {
            conn.sans = crate::tls::extract_sans_from_der(der);
        }
    }

    #[cfg(not(feature = "rustls"))]
    fn populate_sans(_conn: &mut PooledConnection) {}

    fn checkin_connection(&self, key: crate::pool::PoolKey, mut conn: PooledConnection) {
        Self::populate_sans(&mut conn);
        self.fire_connection_metrics(&conn, false);
        self.pool.checkin(key, conn);
    }

    fn fire_connection_metrics(&self, conn: &PooledConnection, closed: bool) {
        if let Some(ref obs) = self.observer
            && let Some(remote_addr) = conn.remote_addr
        {
            obs.on_event(&RequestEvent {
                method: http::Method::GET,
                uri: Uri::default(),
                phase: RequestPhase::ConnectionMetrics {
                    remote_addr,
                    protocol: Self::connection_protocol(conn),
                    bytes_sent: conn.bytes_sent,
                    bytes_received: conn.bytes_received,
                    connection_age: conn.created_at.elapsed(),
                    requests_served: conn.requests_served,
                    closed,
                },
                at: observer::Instant::now(),
            });
        }
    }

    #[inline]
    fn notify(&self, method: &http::Method, uri: &Uri, phase: RequestPhase) {
        if let Some(ref obs) = self.observer {
            obs.on_event(&RequestEvent {
                method: method.clone(),
                uri: uri.clone(),
                phase,
                at: observer::Instant::now(),
            });
        }
    }

    fn attach_observer(&self, resp: &mut Response, method: &http::Method, uri: &Uri) {
        if let Some(ref obs) = self.observer {
            resp.set_observer_ctx(BodyObserverCtx {
                observer: obs.clone(),
                method: method.clone(),
                uri: uri.clone(),
                response_started: Instant::now(),
            });
        }
    }

    fn connection_protocol(conn: &PooledConnection) -> observer::NegotiatedProtocol {
        match &conn.conn {
            HttpConnection::H1(_) => observer::NegotiatedProtocol::Http1,
            HttpConnection::H2(_) => observer::NegotiatedProtocol::Http2,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => observer::NegotiatedProtocol::Http3,
        }
    }

    pub(crate) async fn execute_single(
        &self,
        request: http::Request<AioductBody>,
        original_uri: &Uri,
        replay_body: Option<Bytes>,
    ) -> Result<Response, Error> {
        self.execute_single_with_hint(request, original_uri, ProtocolHint::Auto, replay_body)
            .await
    }

    #[allow(deprecated)] // TimingCollector usage — will be removed when observer replaces it
    pub(crate) async fn execute_single_with_hint(
        &self,
        mut request: http::Request<AioductBody>,
        original_uri: &Uri,
        protocol: ProtocolHint,
        replay_body: Option<Bytes>,
    ) -> Result<Response, Error> {
        let request_start = Instant::now();

        if let Some(ref limiter) = self.rate_limiter {
            while !limiter.try_acquire() {
                let wait = limiter.wait_duration();
                R::sleep(wait).await;
            }
        }

        self.notify(request.method(), original_uri, RequestPhase::Started);
        let pool_checkout_start = Instant::now();

        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        // Resolve AdaptiveH2c via the probe cache
        let effective_protocol = match protocol {
            ProtocolHint::AdaptiveH2c => {
                match self.h2c_probe_cache.lookup(authority) {
                    Some(true) => ProtocolHint::H2c,
                    Some(false) => ProtocolHint::Auto,
                    None => ProtocolHint::AdaptiveH2c, // needs probing
                }
            }
            other => other,
        };
        let force_h2c = matches!(
            effective_protocol,
            ProtocolHint::H2c | ProtocolHint::AdaptiveH2c
        );

        let mut pool_key = crate::pool::PoolKey::with_hint(
            scheme.clone(),
            authority.clone(),
            if force_h2c {
                ProtocolHint::H2c
            } else {
                ProtocolHint::Auto
            },
        );

        let can_stale_retry = !self.no_connection_reuse
            && (http_body::Body::is_end_stream(request.body()) || replay_body.is_some());

        if !self.no_connection_reuse
            && let Some(mut conn) = self.pool.checkout(&pool_key)
        {
            #[cfg(feature = "tracing")]
            tracing::trace!(host = authority.host(), "connection.pool.hit");

            self.notify(
                request.method(),
                original_uri,
                RequestPhase::PoolCheckoutComplete {
                    outcome: observer::PoolOutcome::Hit,
                    blocked_duration: pool_checkout_start.elapsed(),
                },
            );

            let saved_parts = if can_stale_retry {
                Some((
                    request.method().clone(),
                    request.uri().clone(),
                    request.headers().clone(),
                    request.version(),
                ))
            } else {
                None
            };

            let req_method = request.method().clone();
            let transfer_start = Instant::now();
            self.notify(
                &req_method,
                original_uri,
                RequestPhase::RequestSent {
                    duration: transfer_start.duration_since(pool_checkout_start),
                },
            );
            match Self::send_on_connection(&mut conn, request, original_uri.clone()).await {
                Ok(mut resp) => {
                    let transfer = transfer_start.elapsed();
                    self.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::ResponseStarted {
                            waiting_duration: transfer,
                        },
                    );
                    let protocol = Self::connection_protocol(&conn);
                    self.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::ResponseComplete {
                            status: resp.status(),
                            protocol,
                            total_duration: request_start.elapsed(),
                        },
                    );
                    resp.set_remote_addr(conn.remote_addr);
                    resp.set_tls_info(conn.tls_info.clone());
                    resp.set_timings(Some(
                        TimingCollector::default()
                            .into_timings(Some(transfer), request_start.elapsed()),
                    ));
                    self.attach_observer(&mut resp, &req_method, original_uri);
                    if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                        self.checkin_when_ready(pool_key, conn);
                    }
                    return Ok(resp);
                }
                Err(e) if saved_parts.is_some() && Self::is_stale_connection_error(&e) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        host = authority.host(),
                        error = %e,
                        "connection.pool.stale — retrying on fresh connection"
                    );
                    self.fire_connection_metrics(&conn, true);
                    self.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::Failed {
                            error: e.to_string(),
                            will_retry: true,
                            elapsed: request_start.elapsed(),
                        },
                    );
                    self.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::PoolCheckoutComplete {
                            outcome: observer::PoolOutcome::StaleRetry,
                            blocked_duration: pool_checkout_start.elapsed(),
                        },
                    );
                    let (method, uri, headers, version) = saved_parts.unwrap();
                    let retry_body_bytes = replay_body
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(bytes::Bytes::new);
                    let body: AioductBody = http_body_util::Full::new(retry_body_bytes)
                        .map_err(|never| match never {})
                        .boxed_unsync();
                    let mut retry_req = http::Request::new(body);
                    *retry_req.method_mut() = method;
                    *retry_req.uri_mut() = uri;
                    *retry_req.headers_mut() = headers;
                    *retry_req.version_mut() = version;
                    request = retry_req;
                }
                Err(e) => {
                    self.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::Failed {
                            error: e.to_string(),
                            will_retry: false,
                            elapsed: request_start.elapsed(),
                        },
                    );
                    return Err(e);
                }
            }
        }

        // Connection coalescing: try to reuse an h2/h3 connection whose TLS cert
        // covers the target domain via SANs (RFC 7540 §9.1.1).
        if self.connection_coalescing && is_https && !self.no_connection_reuse {
            let port = authority.port_u16().unwrap_or(443);
            let resolved_ip = self
                .resolve_all_authority_raw(authority.host(), port)
                .await
                .ok()
                .and_then(|addrs| addrs.first().map(|a| a.ip()));
            if let Some(mut conn) = self.pool.checkout_coalesced(authority.host(), resolved_ip) {
                #[cfg(feature = "tracing")]
                tracing::trace!(host = authority.host(), "connection.pool.coalesced");

                self.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::PoolCheckoutComplete {
                        outcome: observer::PoolOutcome::Coalesced,
                        blocked_duration: pool_checkout_start.elapsed(),
                    },
                );

                let saved_parts = if can_stale_retry {
                    Some((
                        request.method().clone(),
                        request.uri().clone(),
                        request.headers().clone(),
                        request.version(),
                    ))
                } else {
                    None
                };

                let req_method = request.method().clone();
                let transfer_start = Instant::now();
                self.notify(
                    &req_method,
                    original_uri,
                    RequestPhase::RequestSent {
                        duration: transfer_start.duration_since(pool_checkout_start),
                    },
                );
                match Self::send_on_connection(&mut conn, request, original_uri.clone()).await {
                    Ok(mut resp) => {
                        let transfer = transfer_start.elapsed();
                        self.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::ResponseStarted {
                                waiting_duration: transfer,
                            },
                        );
                        let protocol = Self::connection_protocol(&conn);
                        self.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::ResponseComplete {
                                status: resp.status(),
                                protocol,
                                total_duration: request_start.elapsed(),
                            },
                        );
                        resp.set_remote_addr(conn.remote_addr);
                        resp.set_tls_info(conn.tls_info.clone());
                        resp.set_timings(Some(
                            TimingCollector::default()
                                .into_timings(Some(transfer), request_start.elapsed()),
                        ));
                        self.attach_observer(&mut resp, &req_method, original_uri);
                        if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                            self.checkin_when_ready(pool_key, conn);
                        }
                        return Ok(resp);
                    }
                    Err(e) if saved_parts.is_some() && Self::is_stale_connection_error(&e) => {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            host = authority.host(),
                            error = %e,
                            "connection.pool.coalesced.stale — retrying on fresh connection"
                        );
                        self.fire_connection_metrics(&conn, true);
                        self.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::Failed {
                                error: e.to_string(),
                                will_retry: true,
                                elapsed: request_start.elapsed(),
                            },
                        );
                        self.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::PoolCheckoutComplete {
                                outcome: observer::PoolOutcome::StaleRetry,
                                blocked_duration: pool_checkout_start.elapsed(),
                            },
                        );
                        let (method, uri, headers, version) = saved_parts.unwrap();
                        let retry_body_bytes = replay_body
                            .as_ref()
                            .cloned()
                            .unwrap_or_else(bytes::Bytes::new);
                        let body: AioductBody = http_body_util::Full::new(retry_body_bytes)
                            .map_err(|never| match never {})
                            .boxed_unsync();
                        let mut retry_req = http::Request::new(body);
                        *retry_req.method_mut() = method;
                        *retry_req.uri_mut() = uri;
                        *retry_req.headers_mut() = headers;
                        *retry_req.version_mut() = version;
                        request = retry_req;
                    }
                    Err(e) => {
                        self.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::Failed {
                                error: e.to_string(),
                                will_retry: false,
                                elapsed: request_start.elapsed(),
                            },
                        );
                        return Err(e);
                    }
                }
            }
        }

        #[cfg(all(feature = "http3", feature = "rustls"))]
        if is_https && let Some(endpoint) = &self.h3_endpoint {
            let use_h3 = self.prefer_h3 || self.alt_svc_cache.lookup_h3(authority).is_some();
            if use_h3 {
                self.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::PoolCheckoutComplete {
                        outcome: observer::PoolOutcome::Miss,
                        blocked_duration: pool_checkout_start.elapsed(),
                    },
                );

                let default_port = 443u16;
                let (h3_host, h3_port) = self
                    .alt_svc_cache
                    .lookup_h3(authority)
                    .unwrap_or_else(|| (None, authority.port_u16().unwrap_or(default_port)));
                let connect_host = h3_host.as_deref().unwrap_or(authority.host());
                let dns_start = Instant::now();
                let addrs = self
                    .resolve_all_authority_raw(connect_host, h3_port)
                    .await?;
                self.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::DnsResolved {
                        addrs: addrs.clone(),
                        duration: dns_start.elapsed(),
                    },
                );
                let sni_host = authority.host().to_owned();

                let is_idempotent = matches!(
                    request.method(),
                    &http::Method::GET | &http::Method::HEAD | &http::Method::OPTIONS
                );
                let use_0rtt = self.h3_zero_rtt && is_idempotent;

                let tcp_start = Instant::now();
                let (mut pooled, addr) = if use_0rtt {
                    let (pooled, addr, _used_0rtt) =
                        crate::h3_transport::connect_h3_addrs_0rtt::<R>(
                            endpoint,
                            &addrs,
                            &sni_host,
                            self.local_address,
                        )
                        .await?;
                    (pooled, addr)
                } else {
                    crate::h3_transport::connect_h3_addrs::<R>(
                        endpoint,
                        &addrs,
                        &sni_host,
                        self.local_address,
                    )
                    .await?
                };
                self.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::TcpConnected {
                        remote_addr: addr,
                        duration: tcp_start.elapsed(),
                        protocol: observer::NegotiatedProtocol::Http3,
                    },
                );

                pooled.remote_addr = Some(addr);
                let req_method = request.method().clone();
                let transfer_start = Instant::now();
                self.notify(
                    &req_method,
                    original_uri,
                    RequestPhase::RequestSent {
                        duration: transfer_start.duration_since(pool_checkout_start),
                    },
                );
                let mut resp =
                    Self::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
                let transfer = transfer_start.elapsed();
                self.notify(
                    &req_method,
                    original_uri,
                    RequestPhase::ResponseStarted {
                        waiting_duration: transfer,
                    },
                );
                self.notify(
                    &req_method,
                    original_uri,
                    RequestPhase::ResponseComplete {
                        status: resp.status(),
                        protocol: observer::NegotiatedProtocol::Http3,
                        total_duration: request_start.elapsed(),
                    },
                );
                resp.set_remote_addr(pooled.remote_addr);
                resp.set_tls_info(pooled.tls_info.clone());
                self.attach_observer(&mut resp, &req_method, original_uri);
                if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                    self.checkin_when_ready(pool_key, pooled);
                }
                return Ok(resp);
            }
        }

        self.notify(
            request.method(),
            original_uri,
            RequestPhase::PoolCheckoutComplete {
                outcome: observer::PoolOutcome::Miss,
                blocked_duration: pool_checkout_start.elapsed(),
            },
        );

        let proxy = self
            .proxy
            .as_ref()
            .and_then(|settings| settings.proxy_for(original_uri));

        #[cfg(unix)]
        let unix_socket = self.unix_socket.as_ref();
        #[cfg(not(unix))]
        let unix_socket: Option<&std::path::PathBuf> = None;

        let mut timing = TimingCollector::default();

        let mut pooled = if let Some(unix_path) = unix_socket {
            let _ = &proxy; // suppress unused warning when unix_socket is set
            let _ = unix_path; // suppress unused warning during v0.2 migration
            #[cfg(unix)]
            {
                #[allow(unreachable_code)]
                let connect_fut = async {
                    #[cfg(feature = "tokio")]
                    {
                        let std_stream = std::os::unix::net::UnixStream::connect(unix_path)
                            .map_err(Error::Io)?;
                        std_stream.set_nonblocking(true).map_err(Error::Io)?;
                        let unix_stream =
                            tokio::net::UnixStream::from_std(std_stream).map_err(Error::Io)?;
                        let io = crate::runtime::tokio_rt::TokioIo::new(unix_stream);
                        return self.connect_plaintext_with_hint(io, force_h2c).await;
                    }
                    #[cfg(feature = "smol")]
                    {
                        let unix_stream = smol::net::unix::UnixStream::connect(unix_path)
                            .await
                            .map_err(Error::Io)?;
                        let io = crate::runtime::smol_rt::SmolIo::new(unix_stream);
                        return self.connect_plaintext_with_hint(io, force_h2c).await;
                    }
                    Err::<PooledConnection, Error>(Error::Other(
                        "unix socket support requires tokio or smol feature".into(),
                    ))
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

            let dns_start = Instant::now();
            let addrs = self.resolve_all_authority_raw(host, port).await?;
            timing.dns = Some(dns_start.elapsed());
            self.notify(
                request.method(),
                original_uri,
                RequestPhase::DnsResolved {
                    addrs: addrs.clone(),
                    duration: dns_start.elapsed(),
                },
            );

            let tcp_keepalive = self.tcp_keepalive;
            let tcp_keepalive_interval = self.tcp_keepalive_interval;
            let tcp_keepalive_retries = self.tcp_keepalive_retries;
            let tcp_fast_open = self.tcp_fast_open;
            let local_address = self.local_address;
            #[cfg(target_os = "linux")]
            let interface = self.interface.as_deref();

            let tcp_start = Instant::now();
            let connect_fut = async {
                #[cfg(feature = "tracing")]
                tracing::trace!(addrs = ?addrs, "tcp.connect.start");

                let (tcp_stream, addr) = if addrs.len() > 1 && local_address.is_none() {
                    #[cfg(feature = "tower")]
                    let _ = original_uri;
                    crate::happy_eyeballs::connect_happy_eyeballs::<R, C>(
                        &self.connector,
                        &addrs,
                        local_address,
                    )
                    .await
                    .map_err(Error::Io)?
                } else {
                    let addr = addrs[0];
                    let stream = if let Some(local_addr) = local_address {
                        self.connector
                            .connect_bound(addr, local_addr)
                            .await
                            .map_err(Error::Io)?
                    } else {
                        #[cfg(feature = "tower")]
                        if let Some(ref tower_conn) = self.tower_connector {
                            let info = crate::connector::ConnectInfo {
                                uri: original_uri.clone(),
                                addr,
                            };
                            tower_conn.connect(info).await.map_err(Error::Io)?
                        } else {
                            self.connector.connect(addr).await.map_err(Error::Io)?
                        }
                        #[cfg(not(feature = "tower"))]
                        self.connector.connect(addr).await.map_err(Error::Io)?
                    };
                    (stream, addr)
                };

                #[cfg(target_os = "linux")]
                if let Some(iface) = interface {
                    tcp_stream.bind_device(iface).map_err(Error::Io)?;
                }
                if let Some(time) = tcp_keepalive {
                    tcp_stream
                        .set_keepalive(time, tcp_keepalive_interval, tcp_keepalive_retries)
                        .map_err(Error::Io)?;
                }
                if tcp_fast_open {
                    let _ = tcp_stream.set_fast_open();
                }
                #[cfg(feature = "tracing")]
                tracing::trace!(addr = %addr, "tcp.connect.done");

                let mut conn = if is_https {
                    self.connect_tls(tcp_stream, authority.host()).await?
                } else if matches!(effective_protocol, ProtocolHint::AdaptiveH2c) {
                    // Probe: try h2c, fall back to h1 on failure.
                    // The h2 handshake can "succeed" even against an h1 server
                    // because hyper returns the sender before the server processes
                    // the preface. Wait briefly for the connection driver to detect
                    // a close, then check readiness.
                    let h2c_ok = match self.connect_h2_prior_knowledge(tcp_stream).await {
                        Ok(c) => {
                            R::sleep(std::time::Duration::from_millis(50)).await;
                            if c.is_ready() { Some(c) } else { None }
                        }
                        Err(_) => None,
                    };
                    match h2c_ok {
                        Some(c) => {
                            self.h2c_probe_cache.record_h2c(authority.clone());
                            c
                        }
                        None => {
                            self.h2c_probe_cache.record_h1_only(authority.clone());
                            let stream2 = if addrs.len() > 1 && local_address.is_none() {
                                crate::happy_eyeballs::connect_happy_eyeballs::<R, C>(
                                    &self.connector,
                                    &addrs,
                                    local_address,
                                )
                                .await
                                .map_err(Error::Io)?
                                .0
                            } else {
                                self.connector.connect(addrs[0]).await.map_err(Error::Io)?
                            };
                            self.connect_h1(stream2).await?
                        }
                    }
                } else {
                    self.connect_plaintext_with_hint(tcp_stream, force_h2c)
                        .await?
                };
                conn.remote_addr = Some(addr);
                Ok::<(PooledConnection, Instant), Error>((conn, Instant::now()))
            };

            let (conn, connect_done) = match self.connect_timeout {
                Some(duration) => {
                    crate::timeout::Timeout::WithTimeout {
                        future: connect_fut,
                        sleep: R::sleep(duration),
                    }
                    .await?
                }
                None => connect_fut.await?,
            };
            let tcp_tls_elapsed = connect_done.duration_since(tcp_start);
            if is_https {
                if let Some(tls_dur) = conn.tls_handshake_duration {
                    timing.tls_handshake = Some(tls_dur);
                    timing.tcp_connect = Some(tcp_tls_elapsed.saturating_sub(tls_dur));
                    let tcp_dur = tcp_tls_elapsed.saturating_sub(tls_dur);
                    if let Some(addr) = conn.remote_addr {
                        self.notify(
                            request.method(),
                            original_uri,
                            RequestPhase::TcpConnected {
                                remote_addr: addr,
                                duration: tcp_dur,
                                protocol: Self::connection_protocol(&conn),
                            },
                        );
                    }
                    self.notify(
                        request.method(),
                        original_uri,
                        RequestPhase::TlsHandshakeComplete {
                            duration: tls_dur,
                            alpn_protocol: match &conn.conn {
                                HttpConnection::H2(_) => Some("h2".into()),
                                HttpConnection::H1(_) => Some("http/1.1".into()),
                                #[cfg(all(feature = "http3", feature = "rustls"))]
                                HttpConnection::H3(_) => Some("h3".into()),
                            },
                            peer_certificate_der: conn
                                .tls_info
                                .as_ref()
                                .and_then(|t| t.peer_certificate())
                                .map(|c| c.to_vec()),
                        },
                    );
                } else {
                    timing.tcp_connect = Some(tcp_tls_elapsed);
                    if let Some(addr) = conn.remote_addr {
                        self.notify(
                            request.method(),
                            original_uri,
                            RequestPhase::TcpConnected {
                                remote_addr: addr,
                                duration: tcp_tls_elapsed,
                                protocol: Self::connection_protocol(&conn),
                            },
                        );
                    }
                }
            } else {
                timing.tcp_connect = Some(tcp_tls_elapsed);
                if let Some(addr) = conn.remote_addr {
                    self.notify(
                        request.method(),
                        original_uri,
                        RequestPhase::TcpConnected {
                            remote_addr: addr,
                            duration: tcp_tls_elapsed,
                            protocol: Self::connection_protocol(&conn),
                        },
                    );
                }
            }
            conn
        };

        // Adjust pool key if adaptive probe fell back to h1
        if matches!(protocol, ProtocolHint::AdaptiveH2c)
            && matches!(pooled.conn, HttpConnection::H1(_))
        {
            pool_key.protocol = ProtocolHint::Auto;
        }

        let req_method = request.method().clone();
        let transfer_start = Instant::now();
        self.notify(
            &req_method,
            original_uri,
            RequestPhase::RequestSent {
                duration: transfer_start.duration_since(pool_checkout_start),
            },
        );
        let mut resp = Self::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
        let transfer = transfer_start.elapsed();
        self.notify(
            &req_method,
            original_uri,
            RequestPhase::ResponseStarted {
                waiting_duration: transfer,
            },
        );
        let resp_protocol = Self::connection_protocol(&pooled);
        self.notify(
            &req_method,
            original_uri,
            RequestPhase::ResponseComplete {
                status: resp.status(),
                protocol: resp_protocol,
                total_duration: request_start.elapsed(),
            },
        );
        resp.set_remote_addr(pooled.remote_addr);
        resp.set_tls_info(pooled.tls_info.clone());
        resp.set_timings(Some(
            timing.into_timings(Some(transfer), request_start.elapsed()),
        ));
        self.attach_observer(&mut resp, &req_method, original_uri);
        if !self.no_connection_reuse && resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            self.checkin_when_ready(pool_key, pooled);
        }

        Ok(resp)
    }

    pub(super) async fn send_on_connection(
        conn: &mut PooledConnection,
        request: http::Request<AioductBody>,
        url: Uri,
    ) -> Result<Response, Error> {
        #[cfg(feature = "tracing")]
        let proto = match &conn.conn {
            HttpConnection::H1(_) => "h1",
            HttpConnection::H2(_) => "h2",
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(_) => "h3",
        };
        #[cfg(feature = "tracing")]
        tracing::trace!(
            protocol = proto,
            host = url.host().unwrap_or(""),
            "http.send.start"
        );

        let body_size = http_body::Body::size_hint(request.body())
            .exact()
            .unwrap_or(0);
        conn.bytes_sent += body_size;
        conn.requests_served += 1;

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
            #[cfg(all(feature = "http3", feature = "rustls"))]
            HttpConnection::H3(sender) => {
                crate::h3_transport::send_on_h3(sender, request, url).await
            }
        };

        if let Ok(ref resp) = result
            && let Some(len) = resp.content_length()
        {
            conn.bytes_received += len;
        }

        #[cfg(feature = "tracing")]
        if let Ok(ref resp) = result {
            tracing::trace!(status = resp.status().as_u16(), "http.send.done");
        }

        result
    }

    fn is_stale_connection_error(err: &Error) -> bool {
        match err {
            Error::Hyper(e) => {
                if e.is_canceled() || e.is_closed() || e.is_incomplete_message() {
                    return true;
                }
                // Hyper wraps IO errors (ConnectionReset, BrokenPipe) when the
                // underlying socket fails mid-send on a reused connection.
                use std::error::Error as _;
                if let Some(io_err) = e.source().and_then(|s| s.downcast_ref::<std::io::Error>()) {
                    return matches!(
                        io_err.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::ConnectionAborted
                    );
                }
                false
            }
            Error::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
            ),
            _ => false,
        }
    }
}
