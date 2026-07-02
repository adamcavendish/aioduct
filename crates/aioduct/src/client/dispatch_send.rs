use crate::clock::Instant;

use bytes::Bytes;
use http::Uri;
use http::header::HeaderMap;
use std::time::Duration;

use super::connection_lifecycle::H2ConnectGuard;
use super::{HttpEngineCore, HttpEngineSend};
use crate::body::RequestBodySend;
use crate::error::Error;
use crate::observer::{self, RequestPhase, RetryKind};
use crate::pool::{HttpConnection, PooledConnection, ProtocolHint};
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll, SocketConfig};

use super::extract_headers;
use super::request_replay_send::retry_request_from_parts;

// ── Send path (RuntimePoll + ConnectorSend) ──────────────────────────────────

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_single_with_hint_send(
        &self,
        mut request: http::Request<RequestBodySend>,
        original_uri: &Uri,
        protocol: ProtocolHint,
        replay_body: Option<Bytes>,
        stale_retry_headers: Option<&HeaderMap>,
        connect_timeout: Option<Duration>,
        _write_timeout: Option<Duration>,
        force_addr: Option<std::net::SocketAddr>,
    ) -> Result<Response, Error> {
        let request_start = Instant::now();

        if let Some(ref limiter) = self.core.rate_limiter {
            while !limiter.try_acquire() {
                let wait = limiter.wait_duration();
                R::sleep(wait).await;
            }
        }

        self.core
            .notify(request.method(), original_uri, RequestPhase::Started);
        let pool_checkout_start = Instant::now();

        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        // Resolve AdaptiveH2c via the probe cache.
        let mut effective_protocol = match protocol {
            ProtocolHint::AdaptiveH2c => {
                match self.core.h2c_probe_cache.lookup(authority) {
                    Some(true) => ProtocolHint::H2c,
                    Some(false) => ProtocolHint::Auto,
                    None => ProtocolHint::AdaptiveH2c, // needs probing
                }
            }
            other => other,
        };

        // Through proxies, uncached AdaptiveH2c resolves to Auto (H1):
        // probing requires re-establishing the proxy tunnel on failure,
        // which is disproportionate. Use .h2c() to force h2c through proxies.
        // Must resolve BEFORE pool-key construction so the guard and mark
        // are keyed correctly.
        let through_proxy = self.core.proxy_chain.is_some()
            || self
                .core
                .proxy
                .as_ref()
                .and_then(|s| s.proxy_for(original_uri))
                .is_some();
        if through_proxy && effective_protocol == ProtocolHint::AdaptiveH2c {
            effective_protocol = ProtocolHint::Auto;
        }

        let force_h2c = matches!(
            effective_protocol,
            ProtocolHint::H2c | ProtocolHint::AdaptiveH2c
        );

        // Compute a stable proxy route identity for pool-key segregation.
        // Requests through different proxies (or direct vs proxied) must not
        // share connections.
        let proxy_route = if let Some(ref chain) = self.core.proxy_chain {
            crate::pool::ProxyRoute::from_hash(chain.route_hash())
        } else if let Some(ref config) = self
            .core
            .proxy
            .as_ref()
            .and_then(|s| s.proxy_for(original_uri))
        {
            crate::pool::ProxyRoute::from_hash(config.route_hash())
        } else {
            crate::pool::ProxyRoute::DIRECT
        };

        let mut pool_key = crate::pool::PoolKey::with_hint_and_route(
            scheme.clone(),
            authority.clone(),
            if force_h2c {
                ProtocolHint::H2c
            } else {
                ProtocolHint::Auto
            },
            proxy_route,
        );

        let can_stale_retry = !self.core.no_connection_reuse
            && (http_body::Body::is_end_stream(request.body()) || replay_body.is_some());

        if !self.core.no_connection_reuse
            && let Some(mut conn) = self.core.pool.checkout(&pool_key)
        {
            #[cfg(feature = "tracing")]
            tracing::trace!(host = authority.host(), "connection.pool.hit");

            self.core.pool.record_checkout_hit();

            self.core.notify(
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
                    request.version(),
                ))
            } else {
                None
            };

            let req_method = request.method().clone();
            let transfer_start = Instant::now();
            self.core.notify(
                &req_method,
                original_uri,
                RequestPhase::RequestSent {
                    duration: transfer_start.duration_since(pool_checkout_start),
                    headers: extract_headers(request.headers()),
                },
            );
            match HttpEngineCore::send_on_connection(&mut conn, request, original_uri.clone()).await
            {
                Ok(mut resp) => {
                    let transfer = transfer_start.elapsed();
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::ResponseStarted {
                            waiting_duration: transfer,
                        },
                    );
                    let protocol = HttpEngineCore::connection_protocol(&conn);
                    self.core.notify(
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
                    self.core
                        .attach_observer(&mut resp, &req_method, original_uri);
                    if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(&resp) {
                        self.core.checkin_when_ready::<R, _, _>(
                            pool_key,
                            conn,
                            R::spawn_send,
                            R::sleep(self.core.pool.idle_timeout()),
                        );
                    }
                    return Ok(resp);
                }
                Err(e)
                    if saved_parts.is_some()
                        && HttpEngineCore::<RequestBodySend>::is_stale_connection_error(&e) =>
                {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        host = authority.host(),
                        error = %e,
                        "connection.pool.stale — retrying on fresh connection"
                    );
                    if conn.is_h2_or_h3() {
                        self.core.pool.evict(&pool_key);
                    }
                    self.core.pool.record_stale_reuse_retry();
                    self.core.fire_connection_metrics(&conn, true);
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::Failed {
                            error: e.to_string(),
                            retry: RetryKind::StaleConnection,
                            elapsed: request_start.elapsed(),
                        },
                    );
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::PoolCheckoutComplete {
                            outcome: observer::PoolOutcome::StaleRetry,
                            blocked_duration: pool_checkout_start.elapsed(),
                        },
                    );
                    let Some((method, uri, version)) = saved_parts else {
                        return Err(e);
                    };
                    let headers = stale_retry_headers.cloned().unwrap_or_default();
                    request = retry_request_from_parts(method, uri, version, headers, &replay_body);
                    if let Some(signature) = self
                        .core
                        .prepare_final_request_signature(original_uri, &mut request)?
                    {
                        let signature_headers = signature.sign_send().await?;
                        signature_headers.insert_into(request.headers_mut())?;
                    }
                }
                Err(e) => {
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::Failed {
                            error: e.to_string(),
                            retry: RetryKind::None,
                            elapsed: request_start.elapsed(),
                        },
                    );
                    return Err(e);
                }
            }
        }

        // Connection coalescing: try to reuse an h2/h3 connection whose TLS cert
        // covers the target domain via SANs (RFC 7540 §9.1.1).
        if force_addr.is_none()
            && self.core.connection_coalescing
            && is_https
            && !self.core.no_connection_reuse
        {
            let port = authority.port_u16().unwrap_or(443);
            let resolved_ip = self
                .core
                .resolve_all_authority_raw(authority.host(), port)
                .await
                .ok()
                .and_then(|addrs| addrs.first().map(|a| a.ip()));
            if let Some(mut conn) =
                self.core
                    .pool
                    .checkout_coalesced(authority.host(), resolved_ip, proxy_route)
            {
                #[cfg(feature = "tracing")]
                tracing::trace!(host = authority.host(), "connection.pool.coalesced");

                self.core.pool.record_checkout_coalesced_hit();

                self.core.notify(
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
                        request.version(),
                    ))
                } else {
                    None
                };

                let req_method = request.method().clone();
                let transfer_start = Instant::now();
                self.core.notify(
                    &req_method,
                    original_uri,
                    RequestPhase::RequestSent {
                        duration: transfer_start.duration_since(pool_checkout_start),
                        headers: extract_headers(request.headers()),
                    },
                );
                match HttpEngineCore::send_on_connection(&mut conn, request, original_uri.clone())
                    .await
                {
                    Ok(mut resp) => {
                        let transfer = transfer_start.elapsed();
                        self.core.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::ResponseStarted {
                                waiting_duration: transfer,
                            },
                        );
                        let protocol = HttpEngineCore::connection_protocol(&conn);
                        self.core.notify(
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
                        self.core
                            .attach_observer(&mut resp, &req_method, original_uri);
                        if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(&resp) {
                            self.core.checkin_when_ready::<R, _, _>(
                                pool_key,
                                conn,
                                R::spawn_send,
                                R::sleep(self.core.pool.idle_timeout()),
                            );
                        }
                        return Ok(resp);
                    }
                    Err(e)
                        if saved_parts.is_some()
                            && HttpEngineCore::<RequestBodySend>::is_stale_connection_error(&e) =>
                    {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            host = authority.host(),
                            error = %e,
                            "connection.pool.coalesced.stale — retrying on fresh connection"
                        );
                        if conn.is_h2_or_h3() {
                            let evict_key = conn.key.as_ref().unwrap_or(&pool_key);
                            self.core.pool.evict(evict_key);
                        }
                        self.core.pool.record_stale_reuse_retry();
                        self.core.fire_connection_metrics(&conn, true);
                        self.core.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::Failed {
                                error: e.to_string(),
                                retry: RetryKind::StaleConnection,
                                elapsed: request_start.elapsed(),
                            },
                        );
                        self.core.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::PoolCheckoutComplete {
                                outcome: observer::PoolOutcome::StaleRetry,
                                blocked_duration: pool_checkout_start.elapsed(),
                            },
                        );
                        // saved_parts is guaranteed Some by the match arm guard.
                        let Some((method, uri, version)) = saved_parts else {
                            return Err(e);
                        };
                        let headers = stale_retry_headers.cloned().unwrap_or_default();
                        request =
                            retry_request_from_parts(method, uri, version, headers, &replay_body);
                        if let Some(signature) = self
                            .core
                            .prepare_final_request_signature(original_uri, &mut request)?
                        {
                            let signature_headers = signature.sign_send().await?;
                            signature_headers.insert_into(request.headers_mut())?;
                        }
                    }
                    Err(e) => {
                        self.core.notify(
                            &req_method,
                            original_uri,
                            RequestPhase::Failed {
                                error: e.to_string(),
                                retry: RetryKind::None,
                                elapsed: request_start.elapsed(),
                            },
                        );
                        return Err(e);
                    }
                }
            }
        }

        // When a proxy is configured, never attempt a direct HTTP/3 connection;
        // proxied requests must stay on the configured proxy path (HTTP CONNECT
        // or SOCKS tunnel).  HTTP/3 proxy tunneling (CONNECT-UDP) is not yet
        // supported.
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if is_https
            && !through_proxy
            && let Some(endpoint) = &self.core.h3_endpoint
        {
            let use_h3 =
                self.core.prefer_h3 || self.core.alt_svc_cache.lookup_h3(authority).is_some();
            if use_h3 {
                self.core.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::PoolCheckoutComplete {
                        outcome: observer::PoolOutcome::Miss,
                        blocked_duration: pool_checkout_start.elapsed(),
                    },
                );

                let mut active_reservation = self
                    .core
                    .pool
                    .try_reserve_active(&pool_key)
                    .map_err(Error::from)?;
                self.core.pool.record_checkout_miss();

                let default_port = 443u16;
                let (h3_host, h3_port) = self
                    .core
                    .alt_svc_cache
                    .lookup_h3(authority)
                    .unwrap_or_else(|| (None, authority.port_u16().unwrap_or(default_port)));
                let connect_host = h3_host.as_deref().unwrap_or(authority.host());
                let dns_start = Instant::now();
                let addrs = if let Some(addr) = force_addr {
                    vec![addr]
                } else {
                    self.core
                        .resolve_all_authority_raw(connect_host, h3_port)
                        .await?
                };
                self.core.notify(
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
                let use_0rtt = self.core.h3_zero_rtt && is_idempotent;

                let tcp_start = Instant::now();
                let h3_connect_fut = async {
                    if use_0rtt {
                        let (pooled, addr, _used_0rtt) =
                            crate::h3_transport::connect_h3_addrs_0rtt::<R>(
                                endpoint,
                                &addrs,
                                &sni_host,
                                self.core.local_address,
                            )
                            .await?;
                        Ok((pooled, addr))
                    } else {
                        crate::h3_transport::connect_h3_addrs::<R>(
                            endpoint,
                            &addrs,
                            &sni_host,
                            self.core.local_address,
                        )
                        .await
                    }
                };
                let (mut pooled, addr) =
                    crate::timeout::connect_timeout::<R, _, _>(h3_connect_fut, connect_timeout)
                        .await?;
                self.core.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::TcpConnected {
                        remote_addr: addr,
                        duration: tcp_start.elapsed(),
                        protocol: observer::NegotiatedProtocol::Http3,
                    },
                );

                pooled.remote_addr = Some(addr);
                self.core
                    .pool
                    .attach_active_reservation(&mut pooled, &mut active_reservation);
                let req_method = request.method().clone();
                let transfer_start = Instant::now();
                self.core.notify(
                    &req_method,
                    original_uri,
                    RequestPhase::RequestSent {
                        duration: transfer_start.duration_since(pool_checkout_start),
                        headers: extract_headers(request.headers()),
                    },
                );
                let mut resp =
                    HttpEngineCore::send_on_connection(&mut pooled, request, original_uri.clone())
                        .await?;
                let transfer = transfer_start.elapsed();
                self.core.notify(
                    &req_method,
                    original_uri,
                    RequestPhase::ResponseStarted {
                        waiting_duration: transfer,
                    },
                );
                self.core.notify(
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
                self.core
                    .attach_observer(&mut resp, &req_method, original_uri);
                if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(&resp) {
                    self.core.checkin_when_ready::<R, _, _>(
                        pool_key,
                        pooled,
                        R::spawn_send,
                        R::sleep(self.core.pool.idle_timeout()),
                    );
                }
                return Ok(resp);
            }
        }

        self.core.notify(
            request.method(),
            original_uri,
            RequestPhase::PoolCheckoutComplete {
                outcome: observer::PoolOutcome::Miss,
                blocked_duration: pool_checkout_start.elapsed(),
            },
        );

        // H2/H3 multiplexing: if another task is already establishing an H2
        // connection for this key, wait briefly and retry checkout instead of
        // opening a redundant connection.
        let may_h2 = force_h2c || is_https;
        let mut owns_h2_mark = false;
        if may_h2 && !self.core.no_connection_reuse && {
            let already_marked = self.core.pool.mark_connecting_h2(&pool_key);
            owns_h2_mark = !already_marked;
            already_marked
        } {
            let wait_budget = connect_timeout.unwrap_or(std::time::Duration::from_secs(5));
            let poll_interval = std::time::Duration::from_millis(5);
            let max_polls =
                (wait_budget.as_millis() / poll_interval.as_millis().max(1)).clamp(1, 200);
            for _ in 0..max_polls {
                R::sleep(poll_interval).await;
                if let Some(mut conn) = self.core.pool.checkout(&pool_key) {
                    self.core.pool.record_checkout_hit();
                    self.core.notify(
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
                            request.version(),
                        ))
                    } else {
                        None
                    };
                    let req_method = request.method().clone();
                    let transfer_start = Instant::now();
                    self.core.notify(
                        &req_method,
                        original_uri,
                        RequestPhase::RequestSent {
                            duration: transfer_start.duration_since(pool_checkout_start),
                            headers: extract_headers(request.headers()),
                        },
                    );
                    let result = HttpEngineCore::send_on_connection(
                        &mut conn,
                        request,
                        original_uri.clone(),
                    )
                    .await;
                    match result {
                        Ok(mut resp) => {
                            let transfer = transfer_start.elapsed();
                            let protocol = HttpEngineCore::connection_protocol(&conn);
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::ResponseStarted {
                                    waiting_duration: transfer,
                                },
                            );
                            self.core.notify(
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
                            self.core
                                .attach_observer(&mut resp, &req_method, original_uri);
                            if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(&resp) {
                                self.core.checkin_when_ready::<R, _, _>(
                                    pool_key,
                                    conn,
                                    R::spawn_send,
                                    R::sleep(self.core.pool.idle_timeout()),
                                );
                            }
                            return Ok(resp);
                        }
                        Err(e)
                            if saved_parts.is_some()
                                && HttpEngineCore::<RequestBodySend>::is_stale_connection_error(
                                    &e,
                                ) =>
                        {
                            #[cfg(feature = "tracing")]
                            tracing::debug!(
                                host = authority.host(),
                                error = %e,
                                "connection.pool.stale (h2 wait path) — retrying on fresh connection"
                            );
                            if conn.is_h2_or_h3() {
                                self.core.pool.evict(&pool_key);
                            }
                            self.core.pool.record_stale_reuse_retry();
                            self.core.fire_connection_metrics(&conn, true);
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::Failed {
                                    error: e.to_string(),
                                    retry: RetryKind::StaleConnection,
                                    elapsed: request_start.elapsed(),
                                },
                            );
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::PoolCheckoutComplete {
                                    outcome: observer::PoolOutcome::StaleRetry,
                                    blocked_duration: pool_checkout_start.elapsed(),
                                },
                            );
                            let Some((method, uri, version)) = saved_parts else {
                                return Err(e);
                            };
                            let headers = stale_retry_headers.cloned().unwrap_or_default();
                            request = retry_request_from_parts(
                                method,
                                uri,
                                version,
                                headers,
                                &replay_body,
                            );
                            if let Some(signature) = self
                                .core
                                .prepare_final_request_signature(original_uri, &mut request)?
                            {
                                let signature_headers = signature.sign_send().await?;
                                signature_headers.insert_into(request.headers_mut())?;
                            }
                            break;
                        }
                        Err(e) => {
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::Failed {
                                    error: e.to_string(),
                                    retry: RetryKind::None,
                                    elapsed: request_start.elapsed(),
                                },
                            );
                            return Err(e);
                        }
                    }
                }
            }
            // Timed out waiting — connect ourselves.
            // Just ensure the mark is set; don't unmark first (avoids TOCTOU race).
            owns_h2_mark = !self.core.pool.mark_connecting_h2(&pool_key);
        }

        let mut h2_guard = H2ConnectGuard {
            pool: &self.core.pool,
            key: &pool_key,
            active: may_h2 && owns_h2_mark,
        };

        let proxy = self
            .core
            .proxy
            .as_ref()
            .and_then(|settings| settings.proxy_for(original_uri));

        #[cfg(unix)]
        let unix_socket = self.core.unix_socket.as_ref();
        #[cfg(not(unix))]
        let unix_socket: Option<&std::path::PathBuf> = None;

        let mut active_reservation = self
            .core
            .pool
            .try_reserve_active(&pool_key)
            .map_err(Error::from)?;

        self.core.pool.record_checkout_miss();

        // Through proxies, AdaptiveH2c was already resolved to Auto above
        // (before pool-key construction). proxy_force_h2c is just force_h2c.
        let proxy_force_h2c = force_h2c;

        let mut pooled = if let Some(unix_path) = unix_socket {
            let _ = &proxy;
            // unix_path is unused on non-unix or when neither tokio nor smol is active
            #[cfg(not(all(unix, any(feature = "tokio", feature = "smol"))))]
            let _ = unix_path;
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
                    Err::<PooledConnection<RequestBodySend>, Error>(Error::Other(
                        "unix socket support requires tokio or smol feature".into(),
                    ))
                };
                match connect_timeout {
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
        } else if let Some(ref chain) = self.core.proxy_chain {
            self.connect_via_proxy_chain_send(
                chain,
                authority,
                is_https,
                connect_timeout,
                proxy_force_h2c,
            )
            .await?
        } else if let Some(ref proxy) = proxy {
            self.connect_via_proxy_send(
                proxy,
                authority,
                is_https,
                connect_timeout,
                proxy_force_h2c,
            )
            .await?
        } else {
            let default_port = if is_https { 443 } else { 80 };
            let host = authority.host();
            let port = authority.port_u16().unwrap_or(default_port);

            let dns_start = Instant::now();
            let addrs = if let Some(addr) = force_addr {
                vec![addr]
            } else {
                self.core.resolve_all_authority_raw(host, port).await?
            };
            self.core.notify(
                request.method(),
                original_uri,
                RequestPhase::DnsResolved {
                    addrs: addrs.clone(),
                    duration: dns_start.elapsed(),
                },
            );

            let tcp_keepalive = self.core.tcp_keepalive;
            let tcp_keepalive_interval = self.core.tcp_keepalive_interval;
            let tcp_keepalive_retries = self.core.tcp_keepalive_retries;
            let tcp_fast_open = self.core.tcp_fast_open;
            let local_address = self.core.local_address;
            #[cfg(target_os = "linux")]
            let interface = self.core.interface.as_deref();

            let tcp_start = Instant::now();
            let connect_fut = async {
                #[cfg(feature = "tracing")]
                tracing::trace!(addrs = ?addrs, "tcp.connect.start");

                let (tcp_stream, addr) = if addrs.len() > 1 {
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
                            .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                    } else {
                        #[cfg(feature = "tower")]
                        if let Some(ref tower_slot) = self.tower_connector {
                            let tower_conn = tower_slot.get::<C>();
                            let info = crate::connector::ConnectInfo {
                                uri: original_uri.clone(),
                                addr,
                            };
                            tower_conn
                                .connect(info)
                                .await
                                .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                        } else {
                            self.connector
                                .connect(addr)
                                .await
                                .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                        }
                        #[cfg(not(feature = "tower"))]
                        self.connector
                            .connect(addr)
                            .await
                            .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                    };
                    (stream, addr)
                };

                #[cfg(target_os = "linux")]
                if let Some(iface) = interface {
                    tcp_stream
                        .bind_device(iface)
                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                }
                if let Some(time) = tcp_keepalive {
                    tcp_stream
                        .set_keepalive(time, tcp_keepalive_interval, tcp_keepalive_retries)
                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                }
                if tcp_fast_open {
                    let _ = tcp_stream.set_fast_open();
                }
                #[cfg(feature = "tracing")]
                tracing::trace!(addr = %addr, "tcp.connect.done");

                let mut conn = if is_https {
                    self.connect_tls(tcp_stream, authority.host())
                        .await
                        .map_err(|e| e.with_remote_addr(addr))?
                } else if matches!(effective_protocol, ProtocolHint::AdaptiveH2c) {
                    // Probe: try h2c, fall back to h1 on failure.
                    // The h2 handshake can "succeed" even against an h1 server
                    // because hyper returns the sender before the server processes
                    // the preface. Poll readiness over 200ms to tolerate slow
                    // SETTINGS exchanges and scheduler delays.
                    let h2c_ok = match self.connect_h2_prior_knowledge(tcp_stream).await {
                        Ok(c) => {
                            let mut ready = false;
                            for _ in 0..8 {
                                R::sleep(std::time::Duration::from_millis(25)).await;
                                if c.is_ready() {
                                    ready = true;
                                    break;
                                }
                            }
                            if ready { Some(c) } else { None }
                        }
                        Err(_) => None,
                    };
                    match h2c_ok {
                        Some(c) => {
                            self.core.h2c_probe_cache.record_h2c(authority.clone());
                            c
                        }
                        None => {
                            self.core.h2c_probe_cache.record_h1_only(authority.clone());
                            let (stream2, fallback_addr) = if addrs.len() > 1 {
                                let (s, a) = crate::happy_eyeballs::connect_happy_eyeballs::<R, C>(
                                    &self.connector,
                                    &addrs,
                                    local_address,
                                )
                                .await
                                .map_err(Error::Io)?;
                                (s, a)
                            } else if let Some(local_addr) = local_address {
                                let s = self
                                    .connector
                                    .connect_bound(addrs[0], local_addr)
                                    .await
                                    .map_err(|e| Error::Io(e).with_remote_addr(addrs[0]))?;
                                (s, addrs[0])
                            } else {
                                #[cfg(feature = "tower")]
                                let s = if let Some(ref tower_slot) = self.tower_connector {
                                    let tower_conn = tower_slot.get::<C>();
                                    let info = crate::connector::ConnectInfo {
                                        uri: original_uri.clone(),
                                        addr,
                                    };
                                    tower_conn
                                        .connect(info)
                                        .await
                                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                                } else {
                                    self.connector
                                        .connect(addrs[0])
                                        .await
                                        .map_err(|e| Error::Io(e).with_remote_addr(addrs[0]))?
                                };
                                #[cfg(not(feature = "tower"))]
                                let s = self
                                    .connector
                                    .connect(addrs[0])
                                    .await
                                    .map_err(|e| Error::Io(e).with_remote_addr(addrs[0]))?;
                                (s, addrs[0])
                            };
                            #[cfg(target_os = "linux")]
                            if let Some(iface) = interface {
                                stream2
                                    .bind_device(iface)
                                    .map_err(|e| Error::Io(e).with_remote_addr(fallback_addr))?;
                            }
                            if let Some(time) = tcp_keepalive {
                                stream2
                                    .set_keepalive(
                                        time,
                                        tcp_keepalive_interval,
                                        tcp_keepalive_retries,
                                    )
                                    .map_err(|e| Error::Io(e).with_remote_addr(fallback_addr))?;
                            }
                            if tcp_fast_open {
                                let _ = stream2.set_fast_open();
                            }
                            let mut c = self
                                .connect_h1(stream2)
                                .await
                                .map_err(|e| e.with_remote_addr(fallback_addr))?;
                            c.remote_addr = Some(fallback_addr);
                            c
                        }
                    }
                } else {
                    self.connect_plaintext_with_hint(tcp_stream, force_h2c)
                        .await
                        .map_err(|e| e.with_remote_addr(addr))?
                };
                if conn.remote_addr.is_none() {
                    conn.remote_addr = Some(addr);
                }
                Ok::<(PooledConnection<RequestBodySend>, Instant), Error>((conn, Instant::now()))
            };

            let (conn, connect_done) =
                crate::timeout::connect_timeout::<R, _, _>(connect_fut, connect_timeout).await?;
            let tcp_tls_elapsed = connect_done.duration_since(tcp_start);
            if is_https {
                if let Some(tls_dur) = conn.tls_handshake_duration {
                    let tcp_dur = tcp_tls_elapsed.saturating_sub(tls_dur);
                    if let Some(addr) = conn.remote_addr {
                        self.core.notify(
                            request.method(),
                            original_uri,
                            RequestPhase::TcpConnected {
                                remote_addr: addr,
                                duration: tcp_dur,
                                protocol: HttpEngineCore::connection_protocol(&conn),
                            },
                        );
                    }
                    self.core.notify(
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
                    if let Some(addr) = conn.remote_addr {
                        self.core.notify(
                            request.method(),
                            original_uri,
                            RequestPhase::TcpConnected {
                                remote_addr: addr,
                                duration: tcp_tls_elapsed,
                                protocol: HttpEngineCore::connection_protocol(&conn),
                            },
                        );
                    }
                }
            } else {
                if let Some(addr) = conn.remote_addr {
                    self.core.notify(
                        request.method(),
                        original_uri,
                        RequestPhase::TcpConnected {
                            remote_addr: addr,
                            duration: tcp_tls_elapsed,
                            protocol: HttpEngineCore::connection_protocol(&conn),
                        },
                    );
                }
            }
            conn
        };

        self.core
            .pool
            .attach_active_reservation(&mut pooled, &mut active_reservation);

        // Connection succeeded — deactivate the H2 guard so it won't unmark on
        // drop. The explicit unmark calls below handle the success path.
        h2_guard.active = false;
        drop(h2_guard);

        // Adjust pool key if adaptive probe fell back to h1.
        // Unmark the H2c key BEFORE mutating so the guard state is cleaned
        // up under the original key — not the mutated Auto key.
        if matches!(protocol, ProtocolHint::AdaptiveH2c)
            && matches!(pooled.conn, HttpConnection::H1(_))
        {
            self.core.pool.unmark_connecting_h2(&pool_key);
            // Move the active reservation from the H2c key to the Auto key
            // so check-in/drop decrements the correct counter and subsequent
            // Auto-key requests respect the cap.
            if let Some(ref old_key) = pooled.key {
                let mut new_key = old_key.clone();
                new_key.protocol = ProtocolHint::Auto;
                self.core.pool.rekey_active(old_key, &new_key);
                pooled.key = Some(new_key);
            }
            pool_key.protocol = ProtocolHint::Auto;
        }

        // For H2/H3, check in the original connection immediately so concurrent
        // requests can multiplex onto it, and use a clone for this request.
        // Also, if another concurrent task already established an H2 connection
        // for this key, prefer that (discard the redundant new connection).
        let is_multiplex = pooled.is_h2_or_h3() && !self.core.no_connection_reuse;
        if is_multiplex {
            if let Some(existing) = self.core.pool.checkout(&pool_key) {
                drop(pooled);
                pooled = existing;
            } else if let Some(cloned) = pooled
                .clone_for_multiplex_with_limit(self.core.pool.max_active_streams_per_connection())
            {
                pooled.pool = std::sync::Weak::new();
                pooled.key = None;
                self.core.checkin_connection(pool_key.clone(), pooled);
                pooled = cloned;
            }
            self.core.pool.unmark_connecting_h2(&pool_key);
        } else if may_h2 {
            self.core.pool.unmark_connecting_h2(&pool_key);
        }

        let req_method = request.method().clone();
        let transfer_start = Instant::now();
        self.core.notify(
            &req_method,
            original_uri,
            RequestPhase::RequestSent {
                duration: transfer_start.duration_since(pool_checkout_start),
                headers: extract_headers(request.headers()),
            },
        );
        let mut resp =
            HttpEngineCore::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
        let transfer = transfer_start.elapsed();
        self.core.notify(
            &req_method,
            original_uri,
            RequestPhase::ResponseStarted {
                waiting_duration: transfer,
            },
        );
        let resp_protocol = HttpEngineCore::connection_protocol(&pooled);
        self.core.notify(
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
        self.core
            .attach_observer(&mut resp, &req_method, original_uri);
        if !self.core.no_connection_reuse
            && !HttpEngineCore::<RequestBodySend>::should_skip_checkin(&resp)
        {
            self.core.checkin_when_ready::<R, _, _>(
                pool_key,
                pooled,
                R::spawn_send,
                R::sleep(self.core.pool.idle_timeout()),
            );
        }

        Ok(resp)
    }
}
