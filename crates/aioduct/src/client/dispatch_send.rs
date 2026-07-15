use crate::clock::Instant;

use bytes::Bytes;
use http::Uri;
use std::time::Duration;

use super::connection_lifecycle::{H2ConnectGuard, PooledSendError};
use super::replay::{ReplayReason, RequestReplayPolicy, StaleReplayBudget};
use super::{BodyReplayability, FreshConnectionRequired, HttpEngineCore, HttpEngineSend};
use crate::body::RequestBodySend;
use crate::error::Error;
use crate::h2c_probe::H2cProbeAction;
use crate::observer::{self, RequestPhase, RetryKind};
use crate::pool::{HttpConnection, PooledConnection, ProtocolHint};
use crate::response::Response;
use crate::runtime::{ConnectorSend, RuntimePoll, SocketConfig};

use super::extract_headers;
use super::request_replay::{ReplayableRequestHead, replay_request_send};

fn claim_dispatch_replay(
    budget: &mut StaleReplayBudget,
    policy: RequestReplayPolicy,
    reason: ReplayReason,
    allow_h3_version_fallback: bool,
) -> bool {
    #[cfg(all(feature = "http3", feature = "rustls"))]
    if reason == ReplayReason::VersionFallback && !allow_h3_version_fallback {
        return false;
    }
    #[cfg(not(all(feature = "http3", feature = "rustls")))]
    let _ = allow_h3_version_fallback;
    budget.claim(policy, reason)
}

// ── Send path (RuntimePoll + ConnectorSend) ──────────────────────────────────

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_single_with_hint_send(
        &self,
        mut request: http::Request<RequestBodySend>,
        original_uri: &Uri,
        protocol: ProtocolHint,
        replay_body: Option<Bytes>,
        connect_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
        first_byte_timeout: Option<Duration>,
        force_addr: Option<std::net::SocketAddr>,
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
            protocol,
            None,
        )?;
        let destination = proxy_dispatch_route.destination();
        let scheme = destination.scheme();
        let authority = destination.authority();
        let is_https = scheme == &http::uri::Scheme::HTTPS;
        let through_proxy = proxy_dispatch_route.is_proxied();
        let use_adaptive_h2c =
            protocol == ProtocolHint::AdaptiveH2c && !(through_proxy && is_https);
        let effective_protocol = if use_adaptive_h2c {
            ProtocolHint::AdaptiveH2c
        } else {
            proxy_dispatch_route.protocol_hint()
        };
        let h2c_probe_key = if effective_protocol == ProtocolHint::AdaptiveH2c && !is_https {
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
            if through_proxy && effective_protocol == ProtocolHint::AdaptiveH2c {
                ProtocolHint::H2c
            } else {
                effective_protocol
            },
        )?;
        let proxy_h1_fallback_plan =
            if through_proxy && effective_protocol == ProtocolHint::AdaptiveH2c {
                proxy_dispatch_route.establishment_plan_with_protocol(ProtocolHint::Auto)?
            } else {
                None
            };
        let proxy_route = proxy_dispatch_route.pool_identity();
        if effective_protocol == ProtocolHint::Http3 {
            if through_proxy {
                return Err(Error::Unsupported(
                    "HTTP/3 through a proxy requires CONNECT-UDP and is not supported".to_owned(),
                ));
            }
            if !is_https {
                return Err(Error::Unsupported(
                    "HTTP/3 requires an HTTPS origin".to_owned(),
                ));
            }
            #[cfg(not(all(feature = "http3", feature = "rustls")))]
            return Err(Error::Unsupported(
                "HTTP/3 support is not enabled".to_owned(),
            ));
        }

        let force_h2c = matches!(
            effective_protocol,
            ProtocolHint::Http2 | ProtocolHint::H2c | ProtocolHint::AdaptiveH2c
        );
        let force_h1 = effective_protocol == ProtocolHint::Http1;

        let mut pool_key = crate::pool::PoolKey::with_hint_and_route(
            scheme.clone(),
            authority.clone(),
            match effective_protocol {
                ProtocolHint::Http1 => ProtocolHint::Http1,
                ProtocolHint::Http2 => ProtocolHint::Http2,
                ProtocolHint::Http3 => ProtocolHint::Http3,
                ProtocolHint::H2c | ProtocolHint::AdaptiveH2c => ProtocolHint::H2c,
                ProtocolHint::Auto => ProtocolHint::Auto,
            },
            proxy_route.clone(),
        );
        pool_key.forced_addr = force_addr;
        let adaptive_h1_pool_key = (effective_protocol == ProtocolHint::AdaptiveH2c).then(|| {
            let mut key = pool_key.clone();
            key.protocol = ProtocolHint::Auto;
            key
        });

        let fresh_connection_required = request
            .extensions()
            .get::<FreshConnectionRequired>()
            .is_some();
        #[cfg(all(feature = "http3", feature = "rustls"))]
        let mut h3_alt_svc = if is_https && !through_proxy {
            self.core.alt_svc_cache.lookup_h3(authority)
        } else {
            None
        };
        #[cfg(all(feature = "http3", feature = "rustls"))]
        let h3_dispatch_selected = is_https
            && !through_proxy
            && self.core.h3_endpoint.is_some()
            && (effective_protocol == ProtocolHint::Http3
                || (effective_protocol == ProtocolHint::Auto
                    && (self.core.prefer_h3 || h3_alt_svc.is_some())));
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
        let h3_dispatch_selected = false;
        let replay_policy = RequestReplayPolicy::new(request.method(), body_replayability);
        let mut stale_replay_budget = StaleReplayBudget::default();
        #[cfg(all(feature = "http3", feature = "rustls"))]
        let allow_h3_version_fallback = !self.core.prefer_h3;
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
        let allow_h3_version_fallback = false;
        let can_stale_retry = !self.core.no_connection_reuse
            && replay_policy.permits(ReplayReason::ProvenUnprocessed);
        let can_use_pooled_connection =
            !self.core.no_connection_reuse && !fresh_connection_required;

        #[cfg(all(feature = "http3", feature = "rustls"))]
        if h3_dispatch_selected {
            let (host, port) = h3_alt_svc
                .clone()
                .unwrap_or_else(|| (None, authority.port_u16().unwrap_or(443)));
            pool_key.h3_endpoint = Some((
                host.unwrap_or_else(|| authority.host().to_owned())
                    .to_ascii_lowercase(),
                port,
            ));
        }

        let mut checked_out_pool_key = pool_key.clone();
        let pooled_connection = if can_use_pooled_connection {
            #[cfg(all(feature = "http3", feature = "rustls"))]
            if h3_dispatch_selected {
                self.core.pool.checkout_h3(&pool_key)
            } else {
                self.core.pool.checkout(&pool_key).or_else(|| {
                    adaptive_h1_pool_key.as_ref().and_then(|key| {
                        let connection = self.core.pool.checkout(key);
                        if connection.is_some() {
                            checked_out_pool_key = key.clone();
                        }
                        connection
                    })
                })
            }
            #[cfg(not(all(feature = "http3", feature = "rustls")))]
            self.core.pool.checkout(&pool_key).or_else(|| {
                adaptive_h1_pool_key.as_ref().and_then(|key| {
                    let connection = self.core.pool.checkout(key);
                    if connection.is_some() {
                        checked_out_pool_key = key.clone();
                    }
                    connection
                })
            })
        } else {
            None
        };
        if pooled_connection.is_some() {
            pool_key = checked_out_pool_key;
        }

        if let Some(mut conn) = pooled_connection
            && body_replayability
                .can_start_on_pooled_connection(conn.supports_unsent_request_recovery())
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
                write_timeout,
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
                    HttpEngineCore::<RequestBodySend>::retain_connect_stream_permit(
                        &mut resp,
                        &req_method,
                        &mut conn,
                    );
                    if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(&resp, &req_method) {
                        self.core.checkin_when_ready::<R, _, _>(
                            pool_key,
                            conn,
                            R::spawn_send,
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
                        && HttpEngineCore::<RequestBodySend>::stale_replay_reason(&conn, &e)
                            .is_some_and(|reason| {
                                claim_dispatch_replay(
                                    &mut stale_replay_budget,
                                    replay_policy,
                                    reason,
                                    allow_h3_version_fallback,
                                )
                            }) =>
                {
                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        host = authority.host(),
                        error = %e,
                        "connection.pool.stale — retrying on fresh connection"
                    );
                    #[cfg(all(feature = "http3", feature = "rustls"))]
                    if HttpEngineCore::<RequestBodySend>::h3_failure_invalidates_alt_svc(&conn, &e)
                    {
                        self.core.alt_svc_cache.suppress_h3(authority);
                        h3_alt_svc = None;
                    }
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
                    request = replay_request_send(saved_request, &replay_body);
                    if sign_stale_retries
                        && let Some(signature) = self
                            .core
                            .prepare_final_request_signature(original_uri, &mut request)?
                    {
                        let signature_headers = signature.sign_send().await?;
                        signature_headers.insert_into(request.headers_mut())?;
                    }
                }
                Err(error) => {
                    let e = error.into_error();
                    #[cfg(all(feature = "http3", feature = "rustls"))]
                    if HttpEngineCore::<RequestBodySend>::h3_failure_invalidates_alt_svc(&conn, &e)
                    {
                        self.core.alt_svc_cache.suppress_h3(authority);
                    }
                    if HttpEngineCore::<RequestBodySend>::should_evict_after_send_failure(&conn, &e)
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

        let mut pre_resolved_addrs = None;

        // Connection coalescing: try to reuse an h2/h3 connection whose TLS cert
        // covers the target domain via SANs (RFC 7540 §9.1.1).
        if force_addr.is_none()
            && self.core.connection_coalescing
            && is_https
            && !h3_dispatch_selected
            && !through_proxy
            && can_use_pooled_connection
            && effective_protocol == ProtocolHint::Auto
        {
            let port = authority.port_u16().unwrap_or(443);
            let dns_start = Instant::now();
            let addrs = self
                .core
                .resolve_all_authority_raw(authority.host(), port)
                .await?;
            self.core.notify(
                request.method(),
                original_uri,
                RequestPhase::DnsResolved {
                    addrs: addrs.clone(),
                    duration: dns_start.elapsed(),
                },
            );
            let coalesced = addrs.iter().copied().find_map(|resolved_addr| {
                self.core
                    .pool
                    .checkout_coalesced(authority.host(), resolved_addr, &proxy_route)
            });
            pre_resolved_addrs = Some(addrs);
            if let Some(mut conn) = coalesced
                && body_replayability
                    .can_start_on_pooled_connection(conn.supports_unsent_request_recovery())
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
                    write_timeout,
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
                        HttpEngineCore::<RequestBodySend>::retain_connect_stream_permit(
                            &mut resp,
                            &req_method,
                            &mut conn,
                        );
                        if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(
                            &resp,
                            &req_method,
                        ) {
                            self.core.checkin_when_ready::<R, _, _>(
                                pool_key,
                                conn,
                                R::spawn_send,
                                R::sleep(self.core.pool.idle_timeout()),
                            );
                        }
                        return Ok(resp);
                    }
                    Err(PooledSendError::Recovered {
                        error,
                        request: recovered,
                    }) if replay_policy.permits(ReplayReason::ExactRequestRecovered) => {
                        let evict_key = conn.key.as_ref().unwrap_or(&pool_key);
                        self.core.record_exact_pooled_recovery(
                            &conn,
                            evict_key,
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
                            && HttpEngineCore::<RequestBodySend>::stale_replay_reason(
                                &conn, &e,
                            )
                            .is_some_and(|reason| {
                                claim_dispatch_replay(
                                    &mut stale_replay_budget,
                                    replay_policy,
                                    reason,
                                    allow_h3_version_fallback,
                                )
                            }) =>
                    {
                        #[cfg(feature = "tracing")]
                        tracing::debug!(
                            host = authority.host(),
                            error = %e,
                            "connection.pool.coalesced.stale — retrying on fresh connection"
                        );
                        #[cfg(all(feature = "http3", feature = "rustls"))]
                        if HttpEngineCore::<RequestBodySend>::h3_failure_invalidates_alt_svc(
                            &conn, &e,
                        ) {
                            self.core.alt_svc_cache.suppress_h3(authority);
                        }
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
                        // saved_request is guaranteed Some by the match arm guard.
                        let Some(saved_request) = saved_request else {
                            return Err(e);
                        };
                        request = replay_request_send(saved_request, &replay_body);
                        if sign_stale_retries
                            && let Some(signature) = self
                                .core
                                .prepare_final_request_signature(original_uri, &mut request)?
                        {
                            let signature_headers = signature.sign_send().await?;
                            signature_headers.insert_into(request.headers_mut())?;
                        }
                    }
                    Err(error) => {
                        let e = error.into_error();
                        #[cfg(all(feature = "http3", feature = "rustls"))]
                        if HttpEngineCore::<RequestBodySend>::h3_failure_invalidates_alt_svc(
                            &conn, &e,
                        ) {
                            self.core.alt_svc_cache.suppress_h3(authority);
                        }
                        if HttpEngineCore::<RequestBodySend>::should_evict_after_send_failure(
                            &conn, &e,
                        ) {
                            let evict_key = conn.key.as_ref().unwrap_or(&pool_key);
                            self.core.pool.evict(evict_key);
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

        #[cfg(all(feature = "http3", feature = "rustls"))]
        let mut pool_miss_recorded = false;
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
        let pool_miss_recorded = false;

        // When a proxy is configured, never attempt a direct HTTP/3 connection;
        // proxied requests must stay on the configured proxy path (HTTP CONNECT
        // or SOCKS tunnel).  HTTP/3 proxy tunneling (CONNECT-UDP) is not yet
        // supported.
        #[cfg(all(feature = "http3", feature = "rustls"))]
        'h3_dispatch: {
            if is_https
                && !through_proxy
                && let Some(endpoint) = &self.core.h3_endpoint
            {
                let alt_svc = h3_alt_svc.clone();
                let used_alt_svc = alt_svc.is_some();
                let opportunistic_h3 = effective_protocol == ProtocolHint::Auto
                    && !self.core.prefer_h3
                    && used_alt_svc;
                let use_h3 = effective_protocol == ProtocolHint::Http3
                    || (effective_protocol == ProtocolHint::Auto
                        && (self.core.prefer_h3 || used_alt_svc));
                if !use_h3 {
                    pool_key.h3_endpoint = None;
                    break 'h3_dispatch;
                }
                self.core.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::PoolCheckoutComplete {
                        outcome: observer::PoolOutcome::Miss,
                        blocked_duration: pool_checkout_start.elapsed(),
                    },
                );

                self.core.pool.record_checkout_miss();
                pool_miss_recorded = true;

                let default_port = 443u16;
                let (h3_host, h3_port) =
                    alt_svc.unwrap_or_else(|| (None, authority.port_u16().unwrap_or(default_port)));
                let connect_host = h3_host.as_deref().unwrap_or(authority.host());
                pool_key.h3_endpoint = Some((connect_host.to_ascii_lowercase(), h3_port));
                let can_reuse_pre_resolved = connect_host == authority.host()
                    && h3_port == authority.port_u16().unwrap_or(default_port);
                let dns_start = Instant::now();
                let (addrs, report_dns) = if let Some(addr) = force_addr {
                    (vec![addr], true)
                } else if can_reuse_pre_resolved && let Some(addrs) = pre_resolved_addrs.take() {
                    (addrs, false)
                } else {
                    match self
                        .core
                        .resolve_all_authority_raw(connect_host, h3_port)
                        .await
                    {
                        Ok(addrs) => (addrs, true),
                        Err(error) => {
                            if used_alt_svc {
                                self.core.alt_svc_cache.suppress_h3(authority);
                            }
                            if opportunistic_h3 {
                                break 'h3_dispatch;
                            }
                            return Err(error);
                        }
                    }
                };
                if report_dns {
                    self.core.notify(
                        request.method(),
                        original_uri,
                        RequestPhase::DnsResolved {
                            addrs: addrs.clone(),
                            duration: dns_start.elapsed(),
                        },
                    );
                }
                let sni_host = authority.host().to_owned();

                let saved_request = ReplayableRequestHead::capture(&request);

                loop {
                    let mut active_reservation = self
                        .core
                        .pool
                        .try_reserve_active(&pool_key)
                        .map_err(Error::from)?;
                    let tcp_start = Instant::now();
                    let h3_connect_fut = crate::h3_transport::connect_h3_addrs::<R>(
                        endpoint,
                        &addrs,
                        &sni_host,
                        self.core.local_address,
                    );
                    let (mut pooled, addr) = match crate::timeout::connect_timeout::<R, _, _>(
                        h3_connect_fut,
                        connect_timeout,
                    )
                    .await
                    {
                        Ok(connected) => connected,
                        Err(error) => {
                            if used_alt_svc {
                                self.core.alt_svc_cache.suppress_h3(authority);
                            }
                            if opportunistic_h3 {
                                break 'h3_dispatch;
                            }
                            return Err(error);
                        }
                    };
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
                    let result = Self::send_on_connection_with_first_byte_timeout(
                        &mut pooled,
                        request,
                        original_uri.clone(),
                        write_timeout,
                        first_byte_timeout,
                    )
                    .await;

                    match result {
                        Ok(mut resp) => {
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
                            HttpEngineCore::<RequestBodySend>::retain_connect_stream_permit(
                                &mut resp,
                                &req_method,
                                &mut pooled,
                            );
                            if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(
                                &resp,
                                &req_method,
                            ) {
                                self.core.checkin_when_ready::<R, _, _>(
                                    pool_key,
                                    pooled,
                                    R::spawn_send,
                                    R::sleep(self.core.pool.idle_timeout()),
                                );
                            }
                            return Ok(resp);
                        }
                        Err(error)
                            if HttpEngineCore::<RequestBodySend>::stale_replay_reason(
                                &pooled, &error,
                            ) == Some(ReplayReason::ProvenUnprocessed)
                                && claim_dispatch_replay(
                                    &mut stale_replay_budget,
                                    replay_policy,
                                    ReplayReason::ProvenUnprocessed,
                                    opportunistic_h3,
                                ) =>
                        {
                            self.core.fire_connection_metrics(&pooled, true);
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::Failed {
                                    error: error.to_string(),
                                    retry: RetryKind::StaleConnection,
                                    elapsed: request_start.elapsed(),
                                },
                            );
                            request = replay_request_send(saved_request.clone(), &replay_body);
                            if sign_stale_retries
                                && let Some(signature) = self
                                    .core
                                    .prepare_final_request_signature(original_uri, &mut request)?
                            {
                                let signature_headers = signature.sign_send().await?;
                                signature_headers.insert_into(request.headers_mut())?;
                            }
                        }
                        Err(error)
                            if opportunistic_h3
                                && crate::h3_transport::replay_evidence(&error)
                                    == Some(
                                        crate::h3_transport::H3ReplayEvidence::VersionFallback,
                                    )
                                && claim_dispatch_replay(
                                    &mut stale_replay_budget,
                                    replay_policy,
                                    ReplayReason::VersionFallback,
                                    true,
                                ) =>
                        {
                            self.core.alt_svc_cache.suppress_h3(authority);
                            self.core.fire_connection_metrics(&pooled, true);
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::Failed {
                                    error: error.to_string(),
                                    retry: RetryKind::StaleConnection,
                                    elapsed: request_start.elapsed(),
                                },
                            );
                            request = replay_request_send(saved_request.clone(), &replay_body);
                            if sign_stale_retries
                                && let Some(signature) = self
                                    .core
                                    .prepare_final_request_signature(original_uri, &mut request)?
                            {
                                let signature_headers = signature.sign_send().await?;
                                signature_headers.insert_into(request.headers_mut())?;
                            }
                            break 'h3_dispatch;
                        }
                        Err(error) => {
                            if used_alt_svc && crate::h3_transport::is_endpoint_failure(&error) {
                                self.core.alt_svc_cache.suppress_h3(authority);
                            }
                            self.core.notify(
                                &req_method,
                                original_uri,
                                RequestPhase::Failed {
                                    error: error.to_string(),
                                    retry: RetryKind::None,
                                    elapsed: request_start.elapsed(),
                                },
                            );
                            return Err(error);
                        }
                    }
                }
            }
        }

        #[cfg(all(feature = "http3", feature = "rustls"))]
        if effective_protocol != ProtocolHint::Http3 {
            pool_key.h3_endpoint = None;
        }

        #[cfg(all(feature = "http3", feature = "rustls"))]
        if effective_protocol == ProtocolHint::Http3 {
            return Err(Error::Unsupported(
                "HTTP/3 forwarding requires a client configured with http3(true)".to_owned(),
            ));
        }

        if !pool_miss_recorded {
            self.core.notify(
                request.method(),
                original_uri,
                RequestPhase::PoolCheckoutComplete {
                    outcome: observer::PoolOutcome::Miss,
                    blocked_duration: pool_checkout_start.elapsed(),
                },
            );
        }

        // H2/H3 multiplexing: if another task is already establishing an H2
        // connection for this key, wait briefly and retry checkout instead of
        // opening a redundant connection.
        let may_h2 = !force_h1 && (force_h2c || is_https);
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
                    let result = Self::try_send_on_pooled_connection_with_first_byte_timeout(
                        &mut conn,
                        request,
                        original_uri.clone(),
                        write_timeout,
                        first_byte_timeout,
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
                            HttpEngineCore::<RequestBodySend>::retain_connect_stream_permit(
                                &mut resp,
                                &req_method,
                                &mut conn,
                            );
                            if !HttpEngineCore::<RequestBodySend>::should_skip_checkin(
                                &resp,
                                &req_method,
                            ) {
                                self.core.checkin_when_ready::<R, _, _>(
                                    pool_key,
                                    conn,
                                    R::spawn_send,
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
                                && HttpEngineCore::<RequestBodySend>::stale_replay_reason(
                                    &conn, &e,
                                )
                                .is_some_and(|reason| {
                                    claim_dispatch_replay(
                                        &mut stale_replay_budget,
                                        replay_policy,
                                        reason,
                                        allow_h3_version_fallback,
                                    )
                                }) =>
                        {
                            #[cfg(feature = "tracing")]
                            tracing::debug!(
                                host = authority.host(),
                                error = %e,
                                "connection.pool.stale (h2 wait path) — retrying on fresh connection"
                            );
                            #[cfg(all(feature = "http3", feature = "rustls"))]
                            if HttpEngineCore::<RequestBodySend>::h3_failure_invalidates_alt_svc(
                                &conn, &e,
                            ) {
                                self.core.alt_svc_cache.suppress_h3(authority);
                            }
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
                            request = replay_request_send(saved_request, &replay_body);
                            if sign_stale_retries
                                && let Some(signature) = self
                                    .core
                                    .prepare_final_request_signature(original_uri, &mut request)?
                            {
                                let signature_headers = signature.sign_send().await?;
                                signature_headers.insert_into(request.headers_mut())?;
                            }
                            break;
                        }
                        Err(error) => {
                            let e = error.into_error();
                            #[cfg(all(feature = "http3", feature = "rustls"))]
                            if HttpEngineCore::<RequestBodySend>::h3_failure_invalidates_alt_svc(
                                &conn, &e,
                            ) {
                                self.core.alt_svc_cache.suppress_h3(authority);
                            }
                            if HttpEngineCore::<RequestBodySend>::should_evict_after_send_failure(
                                &conn, &e,
                            ) {
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

        #[cfg(unix)]
        let unix_socket = self.core.unix_socket.as_ref();
        #[cfg(not(unix))]
        let unix_socket: Option<&std::path::PathBuf> = None;

        let mut active_reservation = self
            .core
            .pool
            .try_reserve_active(&pool_key)
            .map_err(Error::from)?;

        if !pool_miss_recorded {
            self.core.pool.record_checkout_miss();
        }

        let request_method = request.method().clone();
        let (mut pooled, pending_h1_probe) = if let Some(unix_path) = unix_socket {
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
                let connection = match connect_timeout {
                    Some(duration) => {
                        crate::timeout::Timeout::WithTimeout {
                            future: connect_fut,
                            sleep: R::sleep(duration),
                        }
                        .await?
                    }
                    None => connect_fut.await?,
                };
                (connection, None)
            }
            #[cfg(not(unix))]
            unreachable!()
        } else if let Some(plan) = proxy_establishment_plan.as_ref() {
            let connect_fut = async {
                if effective_protocol == ProtocolHint::AdaptiveH2c {
                    let fallback_plan = proxy_h1_fallback_plan.as_ref().ok_or_else(|| {
                        Error::Other("adaptive proxy probe is missing its H1 fallback plan".into())
                    })?;
                    let probe_key = h2c_probe_key.as_ref().ok_or_else(|| {
                        Error::Other("adaptive proxy probe is missing its route identity".into())
                    })?;
                    self.connect_via_proxy_plan_adaptive_h2c_send(
                        plan,
                        fallback_plan,
                        &request_method,
                        original_uri,
                        force_addr,
                        probe_key,
                    )
                    .await
                } else {
                    self.connect_via_proxy_plan_send(
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
            let (addrs, report_dns) = if let Some(addr) = force_addr {
                (vec![addr], true)
            } else if let Some(addrs) = pre_resolved_addrs.take() {
                (addrs, false)
            } else {
                (self.core.resolve_all_authority_raw(host, port).await?, true)
            };
            if report_dns {
                self.core.notify(
                    request.method(),
                    original_uri,
                    RequestPhase::DnsResolved {
                        addrs: addrs.clone(),
                        duration: dns_start.elapsed(),
                    },
                );
            }

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

                let mut pending_h1_probe = None;
                let mut conn = if is_https {
                    self.connect_tls_with_hint(tcp_stream, authority.host(), effective_protocol)
                        .await
                        .map_err(|e| e.with_remote_addr(addr))?
                } else if matches!(effective_protocol, ProtocolHint::AdaptiveH2c) {
                    let probe_key = h2c_probe_key.as_ref().ok_or_else(|| {
                        Error::Other("adaptive h2c probe is missing its route identity".into())
                    })?;
                    match self
                        .core
                        .h2c_probe_cache
                        .begin_endpoint_probe(probe_key.endpoint(addr))
                    {
                        H2cProbeAction::UseH1 => self
                            .connect_h1(tcp_stream)
                            .await
                            .map_err(|e| e.with_remote_addr(addr))?,
                        H2cProbeAction::Probe(token) => {
                            let h2c =
                                match self.connect_h2_prior_knowledge_confirmed(tcp_stream).await {
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
                                let stream2 =
                                    if let Some(local_addr) = local_address {
                                        self.connector
                                            .connect_bound(addr, local_addr)
                                            .await
                                            .map_err(|e| Error::Io(e).with_remote_addr(addr))?
                                    } else {
                                        #[cfg(feature = "tower")]
                                        let stream =
                                            if let Some(ref tower_slot) = self.tower_connector {
                                                let tower_conn = tower_slot.get::<C>();
                                                let info = crate::connector::ConnectInfo {
                                                    uri: original_uri.clone(),
                                                    addr,
                                                };
                                                tower_conn.connect(info).await.map_err(|e| {
                                                    Error::Io(e).with_remote_addr(addr)
                                                })?
                                            } else {
                                                self.connector.connect(addr).await.map_err(|e| {
                                                    Error::Io(e).with_remote_addr(addr)
                                                })?
                                            };
                                        #[cfg(not(feature = "tower"))]
                                        let stream = self
                                            .connector
                                            .connect(addr)
                                            .await
                                            .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                                        stream
                                    };
                                #[cfg(target_os = "linux")]
                                if let Some(iface) = interface {
                                    stream2
                                        .bind_device(iface)
                                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                                }
                                if let Some(time) = tcp_keepalive {
                                    stream2
                                        .set_keepalive(
                                            time,
                                            tcp_keepalive_interval,
                                            tcp_keepalive_retries,
                                        )
                                        .map_err(|e| Error::Io(e).with_remote_addr(addr))?;
                                }
                                if tcp_fast_open {
                                    let _ = stream2.set_fast_open();
                                }
                                let mut c = self
                                    .connect_h1(stream2)
                                    .await
                                    .map_err(|e| e.with_remote_addr(addr))?;
                                c.remote_addr = Some(addr);
                                pending_h1_probe = Some(*token);
                                c
                            }
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
            (conn, pending_h1_probe)
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
            if owns_h2_mark {
                self.core.pool.unmark_connecting_h2(&pool_key);
                owns_h2_mark = false;
            }
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
            write_timeout,
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
        HttpEngineCore::<RequestBodySend>::retain_connect_stream_permit(
            &mut resp,
            &req_method,
            &mut pooled,
        );
        if !self.core.no_connection_reuse
            && !HttpEngineCore::<RequestBodySend>::should_skip_checkin(&resp, &req_method)
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

    async fn send_on_connection_with_first_byte_timeout(
        conn: &mut PooledConnection<RequestBodySend>,
        request: http::Request<RequestBodySend>,
        original_uri: Uri,
        write_timeout: Option<Duration>,
        first_byte_timeout: Option<Duration>,
    ) -> Result<Response, Error> {
        let (parts, body) = request.into_parts();
        let (body, request_body_complete) = crate::timeout::mark_body_completion(body);
        let request = http::Request::from_parts(parts, http_body_util::BodyExt::boxed_unsync(body));
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
        let _ = write_timeout;
        #[cfg(all(feature = "http3", feature = "rustls"))]
        let fut = HttpEngineCore::send_on_connection_send::<R>(
            conn,
            request,
            original_uri,
            write_timeout,
        );
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
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
        conn: &mut PooledConnection<RequestBodySend>,
        request: http::Request<RequestBodySend>,
        original_uri: Uri,
        write_timeout: Option<Duration>,
        first_byte_timeout: Option<Duration>,
    ) -> Result<Response, PooledSendError<RequestBodySend>> {
        let (parts, body) = request.into_parts();
        let (body, request_body_complete) = crate::timeout::mark_body_completion(body);
        let request = http::Request::from_parts(parts, http_body_util::BodyExt::boxed_unsync(body));
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
        let _ = write_timeout;
        #[cfg(all(feature = "http3", feature = "rustls"))]
        let future = HttpEngineCore::try_send_on_pooled_connection_send::<R>(
            conn,
            request,
            original_uri,
            write_timeout,
        );
        #[cfg(not(all(feature = "http3", feature = "rustls")))]
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
