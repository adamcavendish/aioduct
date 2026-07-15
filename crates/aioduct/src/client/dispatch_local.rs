use bytes::Bytes;
use http::Uri;
use std::net::SocketAddr;
use std::time::Duration;

use super::connection_lifecycle::{H2ConnectGuard, PooledSendError};
use super::replay::{ReplayReason, RequestReplayPolicy};
use super::request_replay::{ReplayableRequestHead, replay_request_local};
use super::{
    BodyReplayability, FreshConnectionRequired, HttpEngineCore, HttpEngineLocal, extract_headers,
};
use crate::body::RequestBodyLocal;
use crate::clock::Instant;
use crate::error::Error;
use crate::h2c_probe::H2cProbeAction;
use crate::observer::{self, RequestPhase, RetryKind};
use crate::pool::PooledConnection;
use crate::response::Response;
use crate::runtime::{ConnectorLocal, RuntimeLocal, SocketConfig};

fn attach_local_upgrade_handle(
    response: &mut Response,
    method: &http::Method,
    connection: &mut PooledConnection<RequestBodyLocal>,
) {
    let establishes_tunnel = response.status() == http::StatusCode::SWITCHING_PROTOCOLS
        || (*method == http::Method::CONNECT && response.status().is_success());
    if establishes_tunnel && let Some(handle) = connection.upgrade_handle_local.take() {
        response.extensions_mut().insert(handle);
    }
}

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
        first_byte_timeout: Option<Duration>,
        force_addr: Option<SocketAddr>,
        protocol_hint: crate::pool::ProtocolHint,
        sign_stale_retries: bool,
        body_replayability: BodyReplayability,
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

        let proxy_dispatch_route = crate::proxy::ProxyDispatchRoute::resolve(
            original_uri,
            self.core.proxy_chain.as_ref(),
            self.core.proxy.as_ref(),
            protocol_hint,
            None,
        )?;
        let destination = proxy_dispatch_route.destination();
        let scheme = destination.scheme();
        let authority = destination.authority();
        let is_https = scheme == &http::uri::Scheme::HTTPS;
        let through_proxy = proxy_dispatch_route.is_proxied();
        let use_adaptive_h2c =
            protocol_hint == crate::pool::ProtocolHint::AdaptiveH2c && !(through_proxy && is_https);
        let effective_hint = if use_adaptive_h2c {
            crate::pool::ProtocolHint::AdaptiveH2c
        } else {
            proxy_dispatch_route.protocol_hint()
        };
        let h2c_probe_key = if effective_hint == crate::pool::ProtocolHint::AdaptiveH2c && !is_https
        {
            Some(crate::h2c_probe::H2cProbeKey::new(
                scheme.clone(),
                authority.clone(),
                proxy_dispatch_route.pool_identity(),
                force_addr,
            ))
        } else {
            None
        };
        let proxy_establishment_plan = proxy_dispatch_route.establishment_plan_with_protocol(
            if through_proxy && effective_hint == crate::pool::ProtocolHint::AdaptiveH2c {
                crate::pool::ProtocolHint::H2c
            } else {
                effective_hint
            },
        )?;
        let proxy_h1_fallback_plan =
            if through_proxy && effective_hint == crate::pool::ProtocolHint::AdaptiveH2c {
                proxy_dispatch_route
                    .establishment_plan_with_protocol(crate::pool::ProtocolHint::Auto)?
            } else {
                None
            };
        if effective_hint == crate::pool::ProtocolHint::Http3 {
            return Err(Error::Unsupported(
                "HTTP/3 is unavailable on Local runtimes".to_owned(),
            ));
        }

        let force_h2c = matches!(
            effective_hint,
            crate::pool::ProtocolHint::Http2
                | crate::pool::ProtocolHint::H2c
                | crate::pool::ProtocolHint::AdaptiveH2c
        );
        let force_h1 = effective_hint == crate::pool::ProtocolHint::Http1;

        let mut pool_key = crate::pool::PoolKey::with_hint_and_route(
            scheme.clone(),
            authority.clone(),
            match effective_hint {
                crate::pool::ProtocolHint::Http1 => crate::pool::ProtocolHint::Http1,
                crate::pool::ProtocolHint::Http2 => crate::pool::ProtocolHint::Http2,
                crate::pool::ProtocolHint::Http3 => crate::pool::ProtocolHint::Http3,
                crate::pool::ProtocolHint::H2c | crate::pool::ProtocolHint::AdaptiveH2c => {
                    crate::pool::ProtocolHint::H2c
                }
                crate::pool::ProtocolHint::Auto => crate::pool::ProtocolHint::Auto,
            },
            proxy_dispatch_route.pool_identity(),
        );
        pool_key.forced_addr = force_addr;
        let adaptive_h1_pool_key =
            (effective_hint == crate::pool::ProtocolHint::AdaptiveH2c).then(|| {
                let mut key = pool_key.clone();
                key.protocol = crate::pool::ProtocolHint::Auto;
                key
            });
        let may_h2 = !force_h1 && (is_https || force_h2c);

        let fresh_connection_required = request
            .extensions()
            .get::<FreshConnectionRequired>()
            .is_some();
        let replay_policy = RequestReplayPolicy::new(request.method(), body_replayability);
        let can_stale_retry = !self.core.no_connection_reuse
            && replay_policy.permits(ReplayReason::ProvenUnprocessed);
        let can_use_pooled_connection =
            !self.core.no_connection_reuse && !fresh_connection_required;
        let mut checked_out_pool_key = pool_key.clone();
        let pooled_connection = can_use_pooled_connection.then(|| {
            self.core.pool.checkout(&pool_key).or_else(|| {
                adaptive_h1_pool_key.as_ref().and_then(|key| {
                    let connection = self.core.pool.checkout(key);
                    if connection.is_some() {
                        checked_out_pool_key = key.clone();
                    }
                    connection
                })
            })
        });
        if let Some(mut conn) = pooled_connection.flatten()
            && body_replayability
                .can_start_on_pooled_connection(conn.supports_unsent_request_recovery())
        {
            pool_key = checked_out_pool_key;
            self.core.pool.record_checkout_hit();
            self.core.notify(
                request.method(),
                original_uri,
                RequestPhase::PoolCheckoutComplete {
                    outcome: observer::PoolOutcome::Hit,
                    blocked_duration: pool_checkout_start.elapsed(),
                },
            );

            let saved_request = if can_stale_retry {
                Some(ReplayableRequestHead::capture(&request))
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
            match Self::try_send_on_pooled_connection_with_first_byte_timeout(
                &mut conn,
                request,
                original_uri.clone(),
                first_byte_timeout,
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
                    attach_local_upgrade_handle(&mut resp, &req_method, &mut conn);
                    HttpEngineCore::<RequestBodyLocal>::retain_connect_stream_permit(
                        &mut resp,
                        &req_method,
                        &mut conn,
                    );
                    if !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp, &req_method)
                    {
                        self.core.checkin_when_ready_local::<R, _, _>(
                            pool_key,
                            conn,
                            R::spawn_local,
                            R::sleep(self.core.pool.idle_timeout()),
                        );
                    }
                    return Ok(resp);
                }
                Err(PooledSendError::Recovered {
                    error,
                    request: recovered,
                }) if replay_policy.permits(ReplayReason::ExactRequestRecovered) => {
                    self.core.record_exact_pooled_recovery(
                        &conn,
                        &pool_key,
                        &req_method,
                        original_uri,
                        &error,
                        request_start,
                        pool_checkout_start,
                    );
                    request = *recovered;
                }
                Err(PooledSendError::Failed(e))
                    if saved_request.is_some()
                        && HttpEngineCore::<RequestBodyLocal>::stale_replay_reason(&conn, &e)
                            .is_some_and(|reason| replay_policy.permits(reason)) =>
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
                    // saved_request is guaranteed Some by the match arm guard.
                    let Some(saved_request) = saved_request else {
                        return Err(e);
                    };
                    request = replay_request_local(saved_request, &replay_body);
                    if sign_stale_retries
                        && let Some(signature) = self
                            .core
                            .prepare_final_request_signature(original_uri, &mut request)?
                    {
                        let signature_headers = signature.sign_local().await?;
                        signature_headers.insert_into(request.headers_mut())?;
                    }
                }
                Err(error) => {
                    let e = error.into_error();
                    if conn.is_h2_or_h3()
                        && HttpEngineCore::<RequestBodyLocal>::stale_replay_reason(&conn, &e)
                            .is_some()
                    {
                        self.core.pool.evict(&pool_key);
                    }
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
        if may_h2 && can_use_pooled_connection && {
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
                if let Some(mut conn) = self.core.pool.checkout(&pool_key)
                    && body_replayability
                        .can_start_on_pooled_connection(conn.supports_unsent_request_recovery())
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
                    let saved_request = if can_stale_retry {
                        Some(ReplayableRequestHead::capture(&request))
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
                    match Self::try_send_on_pooled_connection_with_first_byte_timeout(
                        &mut conn,
                        request,
                        original_uri.clone(),
                        first_byte_timeout,
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
                            attach_local_upgrade_handle(&mut resp, &req_method, &mut conn);
                            HttpEngineCore::<RequestBodyLocal>::retain_connect_stream_permit(
                                &mut resp,
                                &req_method,
                                &mut conn,
                            );
                            if !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(
                                &resp,
                                &req_method,
                            ) {
                                self.core.checkin_when_ready_local::<R, _, _>(
                                    pool_key,
                                    conn,
                                    R::spawn_local,
                                    R::sleep(self.core.pool.idle_timeout()),
                                );
                            }
                            return Ok(resp);
                        }
                        Err(PooledSendError::Recovered {
                            error,
                            request: recovered,
                        }) if replay_policy.permits(ReplayReason::ExactRequestRecovered) => {
                            self.core.record_exact_pooled_recovery(
                                &conn,
                                &pool_key,
                                &req_method,
                                original_uri,
                                &error,
                                request_start,
                                pool_checkout_start,
                            );
                            request = *recovered;
                            break;
                        }
                        Err(PooledSendError::Failed(e))
                            if saved_request.is_some()
                                && HttpEngineCore::<RequestBodyLocal>::stale_replay_reason(
                                    &conn, &e,
                                )
                                .is_some_and(|reason| replay_policy.permits(reason)) =>
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
                            let Some(saved_request) = saved_request else {
                                return Err(e);
                            };
                            request = replay_request_local(saved_request, &replay_body);
                            if sign_stale_retries
                                && let Some(signature) = self
                                    .core
                                    .prepare_final_request_signature(original_uri, &mut request)?
                            {
                                let signature_headers = signature.sign_local().await?;
                                signature_headers.insert_into(request.headers_mut())?;
                            }
                            break;
                        }
                        Err(error) => {
                            let e = error.into_error();
                            if conn.is_h2_or_h3()
                                && HttpEngineCore::<RequestBodyLocal>::stale_replay_reason(
                                    &conn, &e,
                                )
                                .is_some()
                            {
                                self.core.pool.evict(&pool_key);
                            }
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

        let mut active_reservation = self
            .core
            .pool
            .try_reserve_active(&pool_key)
            .map_err(Error::from)?;

        self.core.pool.record_checkout_miss();

        let request_method = request.method().clone();
        let (mut pooled, pending_h1_probe) = if let Some(plan) = proxy_establishment_plan.as_ref() {
            let connect_fut = async {
                if effective_hint == crate::pool::ProtocolHint::AdaptiveH2c {
                    let fallback_plan = proxy_h1_fallback_plan.as_ref().ok_or_else(|| {
                        Error::Other("adaptive proxy probe is missing its H1 fallback plan".into())
                    })?;
                    let probe_key = h2c_probe_key.as_ref().ok_or_else(|| {
                        Error::Other("adaptive proxy probe is missing its route identity".into())
                    })?;
                    self.connect_via_proxy_plan_adaptive_h2c_local(
                        plan,
                        fallback_plan,
                        &request_method,
                        original_uri,
                        force_addr,
                        probe_key,
                    )
                    .await
                } else {
                    self.connect_via_proxy_plan_local(
                        plan,
                        &request_method,
                        original_uri,
                        force_addr,
                        super::h2_peer_settings::H2PeerSettingsRequirement::NotRequired,
                    )
                    .await
                    .map(|connection| (connection, None))
                }
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
        } else {
            let host = authority.host();
            let port = destination.effective_port();

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
                            .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                    } else {
                        #[cfg(feature = "tower")]
                        if let Some(ref tower_slot) = self.tower_connector_local {
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

                if let Some(time) = self.core.tcp_keepalive {
                    tcp_stream
                        .set_keepalive(
                            time,
                            self.core.tcp_keepalive_interval,
                            self.core.tcp_keepalive_retries,
                        )
                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                }
                if self.core.tcp_fast_open {
                    let _ = tcp_stream.set_fast_open();
                }

                let mut pending_h1_probe = None;
                let mut conn = if is_https {
                    self.connect_tls_local_with_hint(tcp_stream, authority.host(), effective_hint)
                        .await
                        .map_err(|e| e.with_remote_addr(addr))?
                } else if force_h2c && effective_hint == crate::pool::ProtocolHint::AdaptiveH2c {
                    let probe_key = h2c_probe_key.as_ref().ok_or_else(|| {
                        Error::Other("adaptive h2c probe is missing its route identity".into())
                    })?;
                    match self
                        .core
                        .h2c_probe_cache
                        .begin_endpoint_probe(probe_key.endpoint(addr))
                    {
                        H2cProbeAction::UseH1 => self
                            .connect_h1_local(tcp_stream)
                            .await
                            .map_err(|e| e.with_remote_addr(addr))?,
                        H2cProbeAction::Probe(token) => {
                            let h2c = match self
                                .connect_h2_prior_knowledge_local_confirmed(tcp_stream)
                                .await
                            {
                                Ok((connection, confirmation)) => confirmation
                                    .confirmed_within::<R>()
                                    .await
                                    .then_some(connection),
                                Err(_) => None,
                            };
                            if let Some(connection) = h2c {
                                self.core.h2c_probe_cache.confirm_h2c_endpoint(*token);
                                connection
                            } else {
                                self.core.h2c_probe_cache.reject_h2c_endpoint(&token);
                                let stream2 = if let Some(local_addr) = local_address {
                                    self.connector
                                        .connect_bound(addr, local_addr)
                                        .await
                                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                                } else {
                                    #[cfg(feature = "tower")]
                                    let stream =
                                        if let Some(ref tower_slot) = self.tower_connector_local {
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
                                        };
                                    #[cfg(not(feature = "tower"))]
                                    let stream = self
                                        .connector
                                        .connect(addr)
                                        .await
                                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                                    stream
                                };
                                if let Some(time) = self.core.tcp_keepalive {
                                    stream2
                                        .set_keepalive(
                                            time,
                                            self.core.tcp_keepalive_interval,
                                            self.core.tcp_keepalive_retries,
                                        )
                                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                                }
                                if self.core.tcp_fast_open {
                                    let _ = stream2.set_fast_open();
                                }
                                let mut c = self
                                    .connect_h1_local(stream2)
                                    .await
                                    .map_err(|e| e.with_remote_addr(addr))?;
                                c.remote_addr = Some(addr);
                                pending_h1_probe = Some(*token);
                                c
                            }
                        }
                    }
                } else {
                    self.connect_plaintext_local_with_hint(tcp_stream, force_h2c)
                        .await
                        .map_err(|e| e.with_remote_addr(addr))?
                };
                if conn.remote_addr.is_none() {
                    conn.remote_addr = Some(addr);
                }
                Ok::<_, Error>((conn, Instant::now(), pending_h1_probe))
            };

            let (conn, connect_done, pending_h1_probe) =
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
            (conn, pending_h1_probe)
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
            if owns_h2_mark {
                self.core.pool.unmark_connecting_h2(&pool_key);
                owns_h2_mark = false;
            }
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
            if can_use_pooled_connection
                && body_replayability.can_replace_fresh_connection()
                && let Some(existing) = self.core.pool.checkout(&pool_key)
                && body_replayability
                    .can_start_on_pooled_connection(existing.supports_unsent_request_recovery())
            {
                drop(pooled);
                pooled = existing;
            } else if can_use_pooled_connection
                && let Some(cloned) = pooled.clone_for_multiplex_with_limit(
                    self.core.pool.max_active_streams_per_connection(),
                )
            {
                pooled.pool = std::sync::Weak::new();
                pooled.key = None;
                self.core.checkin_connection(pool_key.clone(), pooled);
                pooled = cloned;
            }
        }
        if owns_h2_mark {
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
        let mut resp = Self::send_on_connection_with_first_byte_timeout(
            &mut pooled,
            request,
            original_uri.clone(),
            first_byte_timeout,
        )
        .await?;
        if let Some(token) = pending_h1_probe {
            self.core.h2c_probe_cache.confirm_h1_endpoint(token);
        }
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
        attach_local_upgrade_handle(&mut resp, &req_method, &mut pooled);
        HttpEngineCore::<RequestBodyLocal>::retain_connect_stream_permit(
            &mut resp,
            &req_method,
            &mut pooled,
        );
        if !self.core.no_connection_reuse
            && !HttpEngineCore::<RequestBodyLocal>::should_skip_checkin(&resp, &req_method)
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

    async fn send_on_connection_with_first_byte_timeout(
        conn: &mut PooledConnection<RequestBodyLocal>,
        request: http::Request<RequestBodyLocal>,
        original_uri: Uri,
        first_byte_timeout: Option<Duration>,
    ) -> Result<Response, Error> {
        let (parts, body) = request.into_parts();
        let (body, request_body_complete) = crate::timeout::mark_body_completion(body);
        let request = http::Request::from_parts(parts, Box::pin(body) as RequestBodyLocal);
        let fut = HttpEngineCore::send_on_connection(conn, request, original_uri);
        match first_byte_timeout {
            Some(duration) => {
                match crate::timeout::FirstByteTimeout::<_, R>::new(
                    fut,
                    request_body_complete,
                    duration,
                )
                .await
                {
                    Err(Error::Timeout) => Err(Error::ReadTimeout),
                    other => other,
                }
            }
            None => fut.await,
        }
    }

    async fn try_send_on_pooled_connection_with_first_byte_timeout(
        conn: &mut PooledConnection<RequestBodyLocal>,
        request: http::Request<RequestBodyLocal>,
        original_uri: Uri,
        first_byte_timeout: Option<Duration>,
    ) -> Result<Response, PooledSendError<RequestBodyLocal>> {
        let (parts, body) = request.into_parts();
        let (body, request_body_complete) = crate::timeout::mark_body_completion(body);
        let request = http::Request::from_parts(parts, Box::pin(body) as RequestBodyLocal);
        let future = HttpEngineCore::try_send_on_pooled_connection(conn, request, original_uri);
        match first_byte_timeout {
            Some(duration) => {
                match crate::timeout::FirstByteTimeout::<_, R>::new(
                    future,
                    request_body_complete,
                    duration,
                )
                .await
                {
                    Err(PooledSendError::Failed(Error::Timeout)) => {
                        Err(PooledSendError::Failed(Error::ReadTimeout))
                    }
                    other => other,
                }
            }
            None => future.await,
        }
    }
}
