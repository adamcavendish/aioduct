use bytes::Bytes;
use http::Uri;
use http_body_util::BodyExt;
use std::net::SocketAddr;
use std::time::Duration;

use super::connection_lifecycle::H2ConnectGuard;
use super::{HttpEngineCore, HttpEngineLocal, extract_headers};
use crate::body::RequestBodyLocal;
use crate::clock::Instant;
use crate::error::Error;
use crate::observer::{self, RequestPhase, RetryKind};
use crate::pool::PooledConnection;
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal, SocketConfig};

// ── Local path (RuntimeLocal + ConnectorLocal) ────────────────────────────────────

impl<R: RuntimeLocal, C: ConnectorLocal + Clone> HttpEngineLocal<R, C> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_single_local(
        &self,
        mut request: http::Request<RequestBodyLocal>,
        original_uri: &Uri,
        replay_body: Option<Bytes>,
        connect_timeout: Option<Duration>,
        _write_timeout: Option<Duration>,
        force_addr: Option<SocketAddr>,
        protocol_hint: crate::pool::ProtocolHint,
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

        // Resolve AdaptiveH2c via the probe cache, matching the send-path behavior.
        let mut effective_hint = match protocol_hint {
            crate::pool::ProtocolHint::AdaptiveH2c => {
                match self.core.h2c_probe_cache.lookup(authority) {
                    Some(true) => crate::pool::ProtocolHint::H2c,
                    Some(false) => crate::pool::ProtocolHint::Auto,
                    None => crate::pool::ProtocolHint::AdaptiveH2c,
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
        if through_proxy && effective_hint == crate::pool::ProtocolHint::AdaptiveH2c {
            effective_hint = crate::pool::ProtocolHint::Auto;
        }

        let force_h2c = matches!(
            effective_hint,
            crate::pool::ProtocolHint::H2c | crate::pool::ProtocolHint::AdaptiveH2c
        );

        // Compute a stable proxy route identity for pool-key segregation.
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

        let mut pool_key = if force_h2c {
            crate::pool::PoolKey::with_hint_and_route(
                scheme.clone(),
                authority.clone(),
                crate::pool::ProtocolHint::H2c,
                proxy_route,
            )
        } else {
            crate::pool::PoolKey::with_hint_and_route(
                scheme.clone(),
                authority.clone(),
                crate::pool::ProtocolHint::Auto,
                proxy_route,
            )
        };
        let may_h2 = is_https || force_h2c;

        let can_stale_retry = !self.core.no_connection_reuse
            && (http_body::Body::is_end_stream(request.body()) || replay_body.is_some());
        if !self.core.no_connection_reuse
            && let Some(mut conn) = self.core.pool.checkout(&pool_key)
        {
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
                    request.headers().clone(),
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
                    if let Some(handle) = conn.upgrade_handle_local.take() {
                        resp.extensions_mut().insert(handle);
                    }
                    if !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp) {
                        self.core.checkin_when_ready_local::<R, _, _>(
                            pool_key,
                            conn,
                            R::spawn_local,
                            R::sleep(self.core.pool.idle_timeout()),
                        );
                    }
                    return Ok(resp);
                }
                Err(e)
                    if saved_parts.is_some()
                        && HttpEngineCore::<RequestBodyLocal>::is_stale_connection_error(&e) =>
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
                    // saved_parts is guaranteed Some by the match arm guard.
                    let Some((method, uri, headers, version)) = saved_parts else {
                        return Err(e);
                    };
                    let retry_body_bytes = replay_body
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(bytes::Bytes::new);
                    let body: RequestBodyLocal = Box::pin(
                        http_body_util::Full::new(retry_body_bytes).map_err(|never| match never {}),
                    );
                    let mut retry_req = http::Request::new(body);
                    *retry_req.method_mut() = method;
                    *retry_req.uri_mut() = uri;
                    *retry_req.headers_mut() = headers;
                    *retry_req.version_mut() = version;
                    request = retry_req;
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

        self.core.notify(
            request.method(),
            original_uri,
            RequestPhase::PoolCheckoutComplete {
                outcome: observer::PoolOutcome::Miss,
                blocked_duration: pool_checkout_start.elapsed(),
            },
        );

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
                            request.headers().clone(),
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
                    match HttpEngineCore::send_on_connection(
                        &mut conn,
                        request,
                        original_uri.clone(),
                    )
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
                            if let Some(handle) = conn.upgrade_handle_local.take() {
                                resp.extensions_mut().insert(handle);
                            }
                            if !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp) {
                                self.core
                                    .checkin_when_ready_local::<R, _, _>(pool_key, conn, R::spawn_local, R::sleep(self.core.pool.idle_timeout()));
                            }
                            return Ok(resp);
                        }
                        Err(e)
                            if saved_parts.is_some()
                                && HttpEngineCore::<RequestBodyLocal>::is_stale_connection_error(
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
                            let Some((method, uri, headers, version)) = saved_parts else {
                                return Err(e);
                            };
                            let retry_body_bytes = replay_body
                                .as_ref()
                                .cloned()
                                .unwrap_or_else(bytes::Bytes::new);
                            let body: RequestBodyLocal = Box::pin(
                                http_body_util::Full::new(retry_body_bytes)
                                    .map_err(|never| match never {}),
                            );
                            let mut retry_req = http::Request::new(body);
                            *retry_req.method_mut() = method;
                            *retry_req.uri_mut() = uri;
                            *retry_req.headers_mut() = headers;
                            *retry_req.version_mut() = version;
                            request = retry_req;
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

        let mut active_reservation = self
            .core
            .pool
            .try_reserve_active(&pool_key)
            .map_err(Error::from)?;

        self.core.pool.record_checkout_miss();

        // Through proxies, AdaptiveH2c was already resolved to Auto above
        // (before pool-key construction). proxy_force_h2c is just force_h2c.
        let proxy_force_h2c = force_h2c;

        let mut pooled = if let Some(ref chain) = self.core.proxy_chain {
            self.connect_via_proxy_chain_local(
                chain,
                authority,
                is_https,
                connect_timeout,
                proxy_force_h2c,
            )
            .await?
        } else if let Some(ref proxy) = proxy {
            self.connect_via_proxy_local(
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

            let tcp_start = Instant::now();
            let connect_fut = async {
                let local_address = self.core.local_address;
                let (tcp_stream, addr) = if addrs.len() > 1 {
                    #[cfg(feature = "tower")]
                    let _ = original_uri;
                    crate::happy_eyeballs::connect_happy_eyeballs_local::<R, C>(
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
                        if let Some(ref tower_slot) = self.tower_connector_local {
                            let tower_conn = tower_slot.get::<C>();
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

                let mut conn = if is_https {
                    self.connect_tls_local(tcp_stream, authority.host()).await?
                } else if force_h2c && effective_hint == crate::pool::ProtocolHint::AdaptiveH2c {
                    // Adaptive h2c: try h2c, fall back to H1, cache the result.
                    // Poll readiness over 200ms to tolerate slow SETTINGS
                    // exchanges and scheduler delays.
                    let h2c_ok = match self.connect_h2_prior_knowledge_local(tcp_stream).await {
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
                            let mut c = c;
                            c.remote_addr = Some(addr);
                            c
                        }
                        None => {
                            self.core.h2c_probe_cache.record_h1_only(authority.clone());
                            let (stream2, fallback_addr) = if addrs.len() > 1 {
                                let (s, a) = crate::happy_eyeballs::connect_happy_eyeballs_local::<
                                    R,
                                    C,
                                >(
                                    &self.connector, &addrs, local_address
                                )
                                .await
                                .map_err(Error::Io)?;
                                (s, a)
                            } else if let Some(local_addr) = local_address {
                                let s = self
                                    .connector
                                    .connect_bound(addrs[0], local_addr)
                                    .await
                                    .map_err(Error::Io)?;
                                (s, addrs[0])
                            } else {
                                let s =
                                    self.connector.connect(addrs[0]).await.map_err(Error::Io)?;
                                (s, addrs[0])
                            };
                            if let Some(time) = self.core.tcp_keepalive {
                                stream2
                                    .set_keepalive(
                                        time,
                                        self.core.tcp_keepalive_interval,
                                        self.core.tcp_keepalive_retries,
                                    )
                                    .map_err(Error::Io)?;
                            }
                            if self.core.tcp_fast_open {
                                let _ = stream2.set_fast_open();
                            }
                            let mut c = self.connect_h1_local(stream2).await?;
                            c.remote_addr = Some(fallback_addr);
                            c
                        }
                    }
                } else {
                    self.connect_plaintext_local_with_hint(tcp_stream, force_h2c)
                        .await?
                };
                if conn.remote_addr.is_none() {
                    conn.remote_addr = Some(addr);
                }
                Ok::<(PooledConnection<RequestBodyLocal>, Instant), Error>((conn, Instant::now()))
            };

            let (conn, connect_done) = match connect_timeout {
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
                                crate::pool::HttpConnection::H2(_) => Some("h2".into()),
                                crate::pool::HttpConnection::H1(_) => Some("http/1.1".into()),
                                #[cfg(all(feature = "http3", feature = "rustls"))]
                                crate::pool::HttpConnection::H3(_) => Some("h3".into()),
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

        // H2 multiplexing: deactivate the guard and handle connection sharing
        h2_guard.active = false;
        drop(h2_guard);

        // Adjust pool key if adaptive probe fell back to h1.
        // Unmark the H2c key BEFORE mutating so the guard state is cleaned
        // up under the original key — not the mutated Auto key.
        if matches!(protocol_hint, crate::pool::ProtocolHint::AdaptiveH2c)
            && matches!(pooled.conn, crate::pool::HttpConnection::H1(_))
        {
            self.core.pool.unmark_connecting_h2(&pool_key);
            // Move the active reservation from the H2c key to the Auto key
            // so check-in/drop decrements the correct counter and subsequent
            // Auto-key requests respect the cap.
            if let Some(ref old_key) = pooled.key {
                let mut new_key = old_key.clone();
                new_key.protocol = crate::pool::ProtocolHint::Auto;
                self.core.pool.rekey_active(old_key, &new_key);
                pooled.key = Some(new_key);
            }
            pool_key.protocol = crate::pool::ProtocolHint::Auto;
        }

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
        if let Some(handle) = pooled.upgrade_handle_local.take() {
            resp.extensions_mut().insert(handle);
        }
        if !self.core.no_connection_reuse
            && !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp)
        {
            self.core.checkin_when_ready_local::<R, _, _>(
                pool_key,
                pooled,
                R::spawn_local,
                R::sleep(self.core.pool.idle_timeout()),
            );
        }

        Ok(resp)
    }
}
