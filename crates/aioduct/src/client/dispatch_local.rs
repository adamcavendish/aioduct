use bytes::Bytes;
use http::Uri;
use std::net::SocketAddr;
use std::time::Duration;

use super::connection_deadline::ConnectionDeadline;
use super::connection_lifecycle::{H2ConnectGuard, PooledSendError};
use super::replay::{ReplayReason, RequestReplayPolicy};
use super::request_replay::{ReplayableRequestHead, replay_request_local};
use super::{BodyReplayability, FreshConnectionRequired, HttpEngineCore, HttpEngineLocal};
use crate::body::RequestBodyLocal;
use crate::error::Error;
use crate::h2c_probe::H2cProbeAction;
use crate::observer;
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

fn observe_request_source(
    request: http::Request<RequestBodyLocal>,
    observer: &Option<crate::observer::LocalAttemptObserver>,
) -> http::Request<RequestBodyLocal> {
    match observer
        .as_ref()
        .filter(|observer| observer.request_body_enabled())
    {
        Some(observer) => {
            request.map(|body| crate::body::observe_request_body_local(body, observer.clone()))
        }
        None => request,
    }
}

impl<R: RuntimeLocal, C: ConnectorLocal + Clone, O> HttpEngineLocal<R, C, O>
where
    O: crate::observer::Observer + 'static,
{
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
        attempt_observer: Option<crate::observer::LocalAttemptObserver>,
    ) -> Result<Response, Error> {
        HttpEngineCore::<RequestBodyLocal>::start_observed_attempt(
            &mut request,
            original_uri,
            &attempt_observer,
        );
        request = observe_request_source(request, &attempt_observer);
        if let Some(ref limiter) = self.core.rate_limiter {
            let admission = attempt_observer
                .as_ref()
                .and_then(|observer| observer.rate_limit());
            while !limiter.try_acquire() {
                let wait = limiter.wait_duration();
                R::sleep(wait).await;
            }
            if let Some(admission) = admission {
                admission.admitted();
            }
        }

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
        if !can_use_pooled_connection && let Some(observer) = attempt_observer.as_ref() {
            observer.acquisition_skipped(
                observer::ConnectionAcquisitionPhase::PoolLookup,
                observer::PhaseSkipReason::BypassedByConfiguration,
            );
        }
        let mut checked_out_pool_key = pool_key.clone();
        if can_use_pooled_connection
            && let Some(observer) = attempt_observer.as_ref()
            && observer.acquisition_enabled()
        {
            observer.acquisition_started(observer::ConnectionAcquisitionPhase::PoolLookup);
        }
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
        if can_use_pooled_connection
            && let Some(observer) = attempt_observer.as_ref()
            && observer.acquisition_enabled()
        {
            observer.acquisition_completed(observer::ConnectionAcquisitionPhase::PoolLookup);
            observer.pool_lookup(if pooled_connection.as_ref().is_some_and(Option::is_some) {
                observer::PoolLookupResult::Hit
            } else {
                observer::PoolLookupResult::Miss
            });
        }
        if let Some(mut conn) = pooled_connection.flatten()
            && body_replayability
                .can_start_on_pooled_connection(conn.supports_unsent_request_recovery())
        {
            pool_key = checked_out_pool_key;
            self.core.pool.record_checkout_hit();

            let saved_request = if can_stale_retry {
                Some(ReplayableRequestHead::capture(&request))
            } else {
                None
            };

            let req_method = request.method().clone();
            if let Some(observer) = attempt_observer.as_ref()
                && observer.acquisition_enabled()
            {
                observer.acquisition_acquired(
                    if conn.is_h2_or_h3() {
                        observer::ConnectionAcquisitionResource::Stream
                    } else {
                        observer::ConnectionAcquisitionResource::Connection
                    },
                    conn.metrics.association(),
                    conn.version(),
                );
            }
            match Self::try_send_on_pooled_connection_with_first_byte_timeout(
                &mut conn,
                request,
                original_uri.clone(),
                first_byte_timeout,
                attempt_observer.clone(),
            )
            .await
            {
                Ok(mut resp) => {
                    resp.set_remote_addr(conn.remote_addr);
                    resp.set_tls_info(conn.tls_info.clone());
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
                    self.core
                        .record_exact_pooled_recovery(&conn, &pool_key, original_uri, &error);
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
                    if let Some(observer) = attempt_observer.as_ref() {
                        observer.stale_retry(&e);
                    }
                    // saved_request is guaranteed Some by the match arm guard.
                    let Some(saved_request) = saved_request else {
                        return Err(e);
                    };
                    request = replay_request_local(saved_request, &replay_body);
                    HttpEngineCore::<RequestBodyLocal>::start_observed_attempt(
                        &mut request,
                        original_uri,
                        &attempt_observer,
                    );
                    request = observe_request_source(request, &attempt_observer);
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
                    return Err(e);
                }
            }
        }

        let mut connection_deadline = ConnectionDeadline::new(connect_timeout);
        let mut owns_h2_mark = false;
        if may_h2 && can_use_pooled_connection && {
            let already_marked = self.core.pool.mark_connecting_h2(&pool_key);
            owns_h2_mark = !already_marked;
            already_marked
        } {
            let pool_wait_observed = attempt_observer
                .as_ref()
                .is_some_and(|observer| observer.acquisition_enabled());
            if pool_wait_observed && let Some(observer) = attempt_observer.as_ref() {
                observer.acquisition_started(observer::ConnectionAcquisitionPhase::PoolWait);
            }
            let poll_interval = std::time::Duration::from_millis(5);
            let max_polls = if connect_timeout.is_some() {
                usize::MAX
            } else {
                200
            };
            for _ in 0..max_polls {
                if let Err(error) = connection_deadline.sleep::<R>(poll_interval).await {
                    if let Some(observer) = attempt_observer.as_ref()
                        && pool_wait_observed
                    {
                        observer.acquisition_failed(
                            observer::ConnectionAcquisitionPhase::PoolWait,
                            &error,
                        );
                    }
                    return Err(error);
                }
                if let Some(mut conn) = self.core.pool.checkout(&pool_key)
                    && body_replayability
                        .can_start_on_pooled_connection(conn.supports_unsent_request_recovery())
                {
                    if let Some(observer) = attempt_observer.as_ref()
                        && pool_wait_observed
                    {
                        observer
                            .acquisition_completed(observer::ConnectionAcquisitionPhase::PoolWait);
                    }
                    self.core.pool.record_checkout_hit();
                    let saved_request = if can_stale_retry {
                        Some(ReplayableRequestHead::capture(&request))
                    } else {
                        None
                    };
                    let req_method = request.method().clone();
                    if let Some(observer) = attempt_observer.as_ref()
                        && observer.acquisition_enabled()
                    {
                        observer.acquisition_acquired(
                            if conn.is_h2_or_h3() {
                                observer::ConnectionAcquisitionResource::Stream
                            } else {
                                observer::ConnectionAcquisitionResource::Connection
                            },
                            conn.metrics.association(),
                            conn.version(),
                        );
                    }
                    match Self::try_send_on_pooled_connection_with_first_byte_timeout(
                        &mut conn,
                        request,
                        original_uri.clone(),
                        first_byte_timeout,
                        attempt_observer.clone(),
                    )
                    .await
                    {
                        Ok(mut resp) => {
                            resp.set_remote_addr(conn.remote_addr);
                            resp.set_tls_info(conn.tls_info.clone());
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
                                original_uri,
                                &error,
                            );
                            request = *recovered;
                            connection_deadline = ConnectionDeadline::new(connect_timeout);
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
                            if let Some(observer) = attempt_observer.as_ref() {
                                observer.stale_retry(&e);
                            }
                            let Some(saved_request) = saved_request else {
                                return Err(e);
                            };
                            request = replay_request_local(saved_request, &replay_body);
                            HttpEngineCore::<RequestBodyLocal>::start_observed_attempt(
                                &mut request,
                                original_uri,
                                &attempt_observer,
                            );
                            request = observe_request_source(request, &attempt_observer);
                            if sign_stale_retries
                                && let Some(signature) = self
                                    .core
                                    .prepare_final_request_signature(original_uri, &mut request)?
                            {
                                let signature_headers = signature.sign_local().await?;
                                signature_headers.insert_into(request.headers_mut())?;
                            }
                            connection_deadline = ConnectionDeadline::new(connect_timeout);
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
                            return Err(e);
                        }
                    }
                }
            }
            if let Some(observer) = attempt_observer.as_ref()
                && pool_wait_observed
            {
                observer.acquisition_cancelled(observer::ConnectionAcquisitionPhase::PoolWait);
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
        connection_deadline.check()?;

        self.core.pool.record_checkout_miss();

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
                        force_addr,
                        probe_key,
                        attempt_observer.as_ref(),
                        connection_deadline,
                    )
                    .await
                } else {
                    self.connect_via_proxy_plan_local(
                        plan,
                        force_addr,
                        super::h2_peer_settings::H2PeerSettingsRequirement::NotRequired,
                        attempt_observer.as_ref(),
                        connection_deadline,
                    )
                    .await
                    .map(|connection| (connection, None))
                }
            };
            connect_fut.await?
        } else {
            let host = authority.host();
            let port = destination.effective_port();

            let mut connection_observation = self.begin_connection_observation();
            let establishment = connection_observation
                .as_ref()
                .and_then(|owner| owner.establishment_scope());
            let addrs = if let Some(addr) = force_addr {
                if let Some(observer) = attempt_observer.as_ref() {
                    observer.acquisition_skipped(
                        observer::ConnectionAcquisitionPhase::Dns,
                        observer::PhaseSkipReason::BypassedByConfiguration,
                    );
                }
                vec![addr]
            } else {
                if let Some(observation) = connection_observation.as_ref()
                    && observation
                        .interest()
                        .contains(observer::Interest::CONNECTION_ESTABLISHMENT)
                {
                    observation.emit(&observer::ConnectionObservationEvent::PhaseStarted {
                        phase: observer::ConnectionEstablishmentPhase::Dns,
                        at: observer::Instant::now(),
                    });
                }
                let mut dns_acquisition = attempt_observer.as_ref().and_then(|observer| {
                    observer.acquisition_scope(observer::ConnectionAcquisitionPhase::Dns)
                });
                let resolved = connection_deadline
                    .run::<R, _, _>(self.core.resolve_all_authority_raw(host, port))
                    .await;
                if let Some(acquisition) = &mut dns_acquisition {
                    acquisition.result(&resolved);
                }
                let addrs = match resolved {
                    Ok(addrs) => addrs,
                    Err(error) => {
                        if let Some(observation) = connection_observation.as_ref() {
                            observation.establishment_failed(&error);
                        }
                        return Err(error);
                    }
                };
                if let Some(observation) = connection_observation.as_ref()
                    && observation
                        .interest()
                        .contains(observer::Interest::CONNECTION_ESTABLISHMENT)
                {
                    observation.emit(&observer::ConnectionObservationEvent::PhaseCompleted {
                        phase: observer::ConnectionEstablishmentPhase::Dns,
                        at: observer::Instant::now(),
                    });
                }
                addrs
            };

            let mut tcp_acquisition = attempt_observer.as_ref().and_then(|observer| {
                observer.acquisition_scope(observer::ConnectionAcquisitionPhase::TcpConnect)
            });
            let mut tls_acquisition =
                crate::observer::AcquisitionPhases::new(attempt_observer.as_ref());
            let mut fallback_establishment = None;
            let connect_fut = async {
                if let Some(observation) = connection_observation.as_ref()
                    && observation
                        .interest()
                        .contains(observer::Interest::CONNECTION_ESTABLISHMENT)
                {
                    observation.emit(&observer::ConnectionObservationEvent::PhaseStarted {
                        phase: observer::ConnectionEstablishmentPhase::TcpConnect,
                        at: observer::Instant::now(),
                    });
                }
                let local_address = self.core.local_address;
                let (tcp_stream, addr) = if addrs.len() > 1 {
                    #[cfg(feature = "tower")]
                    let _ = original_uri;
                    if let Some(observation) = connection_observation.as_ref()
                        && observation
                            .interest()
                            .contains(observer::Interest::CONNECTION_ESTABLISHMENT)
                    {
                        crate::happy_eyeballs::connect_happy_eyeballs_local_observed::<R, C, _>(
                            &self.connector,
                            &addrs,
                            local_address,
                            observation,
                        )
                        .await?
                    } else {
                        crate::happy_eyeballs::connect_happy_eyeballs_local::<R, C>(
                            &self.connector,
                            &addrs,
                            local_address,
                        )
                        .await
                        .map_err(Error::Io)?
                    }
                } else {
                    let addr = addrs[0];
                    let mut candidate = connection_observation.as_ref().and_then(|observation| {
                        observation.connect_attempt_scope(observer::ConnectAttempt {
                            sequence: 0,
                            kind: observer::TransportKind::Tcp,
                            remote_addr: addr,
                        })
                    });
                    let stream = if let Some(local_addr) = local_address {
                        self.connector
                            .connect_bound(addr, local_addr)
                            .await
                            .map_err(|e| Error::Io(e).with_remote_addr(addr))
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
                                .map_err(|e| Error::Io(e).with_remote_addr(addr))
                        } else {
                            self.connector
                                .connect(addr)
                                .await
                                .map_err(|e| Error::Io(e).with_remote_addr(addr))
                        }
                        #[cfg(not(feature = "tower"))]
                        self.connector
                            .connect(addr)
                            .await
                            .map_err(|e| Error::Io(e).with_remote_addr(addr))
                    };
                    match &stream {
                        Ok(_) => {
                            if let Some(candidate) = &mut candidate {
                                candidate.succeeded();
                            }
                        }
                        Err(error) => {
                            if let Some(candidate) = &mut candidate {
                                candidate.failed(error);
                            }
                        }
                    }
                    let stream = stream?;
                    (stream, addr)
                };
                if let Some(observation) = connection_observation.as_ref()
                    && observation
                        .interest()
                        .contains(observer::Interest::CONNECTION_ESTABLISHMENT)
                {
                    observation.emit(&observer::ConnectionObservationEvent::PhaseCompleted {
                        phase: observer::ConnectionEstablishmentPhase::TcpConnect,
                        at: observer::Instant::now(),
                    });
                }
                if let Some(acquisition) = &mut tcp_acquisition {
                    acquisition.complete();
                }

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
                    tls_acquisition.start(observer::ConnectionAcquisitionPhase::TlsHandshake);
                    self.connect_tls_local_with_hint_observed_with_owner(
                        tcp_stream,
                        authority.host(),
                        effective_hint,
                        connection_observation.take(),
                        |_, _| tls_acquisition.complete(),
                    )
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
                            .connect_h1_local_with_observation(
                                tcp_stream,
                                connection_observation.take(),
                            )
                            .await
                            .map_err(|e| e.with_remote_addr(addr))?,
                        H2cProbeAction::Probe(token) => {
                            let h2c = self
                                .connect_h2_prior_knowledge_local_confirmed_with_observation(
                                    tcp_stream,
                                    connection_observation.take(),
                                )
                                .await
                                .ok();
                            if let Some(connection) = h2c {
                                self.core.h2c_probe_cache.confirm_h2c_endpoint(*token);
                                connection
                            } else {
                                self.core.h2c_probe_cache.reject_h2c_endpoint(&token);
                                let mut fallback_observation = self.begin_connection_observation();
                                fallback_establishment = fallback_observation
                                    .as_ref()
                                    .and_then(|owner| owner.establishment_scope());
                                tls_acquisition
                                    .start(observer::ConnectionAcquisitionPhase::TcpConnect);
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
                                tls_acquisition.complete();
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
                                    .connect_h1_local_with_observation(
                                        stream2,
                                        fallback_observation.take(),
                                    )
                                    .await
                                    .map_err(|e| e.with_remote_addr(addr))?;
                                c.remote_addr = Some(addr);
                                pending_h1_probe = Some(*token);
                                c
                            }
                        }
                    }
                } else {
                    self.connect_plaintext_local_with_hint_observed(
                        tcp_stream,
                        force_h2c,
                        connection_observation.take(),
                    )
                    .await
                    .map_err(|e| e.with_remote_addr(addr))?
                };
                if conn.remote_addr.is_none() {
                    conn.remote_addr = Some(addr);
                }
                Ok::<_, Error>((conn, pending_h1_probe))
            };

            let result = connection_deadline.run::<R, _, _>(connect_fut).await;
            tls_acquisition.finish(result.as_ref().err());
            if let Some(establishment) = &fallback_establishment {
                establishment.result(&result);
            }
            if let Some(acquisition) = &mut tcp_acquisition {
                acquisition.result(&result);
            }
            if let Some(establishment) = &establishment {
                establishment.result(&result);
            }
            let (conn, pending_h1_probe) = result?;
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
                self.core.pool.ensure_reaper_local::<R>();
                self.core.checkin_connection(pool_key.clone(), pooled);
                pooled = cloned;
            }
        }
        if owns_h2_mark {
            self.core.pool.unmark_connecting_h2(&pool_key);
        }

        let req_method = request.method().clone();
        if let Some(observer) = attempt_observer.as_ref()
            && observer.acquisition_enabled()
        {
            observer.acquisition_acquired(
                if pooled.is_h2_or_h3() {
                    observer::ConnectionAcquisitionResource::Stream
                } else {
                    observer::ConnectionAcquisitionResource::Connection
                },
                pooled.metrics.association(),
                pooled.version(),
            );
        }
        let mut resp = Self::send_on_connection_with_first_byte_timeout(
            &mut pooled,
            request,
            original_uri.clone(),
            first_byte_timeout,
            attempt_observer.clone(),
        )
        .await?;
        if let Some(token) = pending_h1_probe {
            self.core.h2c_probe_cache.confirm_h1_endpoint(token);
        }
        resp.set_remote_addr(pooled.remote_addr);
        resp.set_tls_info(pooled.tls_info.clone());
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
        attempt_observer: Option<crate::observer::LocalAttemptObserver>,
    ) -> Result<Response, Error> {
        let (parts, body) = request.into_parts();
        let (body, request_body_complete) = crate::timeout::mark_body_completion(body);
        let request = http::Request::from_parts(parts, Box::pin(body) as RequestBodyLocal);
        let fut = HttpEngineCore::send_on_connection(conn, request, original_uri, attempt_observer);
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
        attempt_observer: Option<crate::observer::LocalAttemptObserver>,
    ) -> Result<Response, PooledSendError<RequestBodyLocal>> {
        let (parts, body) = request.into_parts();
        let (body, request_body_complete) = crate::timeout::mark_body_completion(body);
        let request = http::Request::from_parts(parts, Box::pin(body) as RequestBodyLocal);
        let future = HttpEngineCore::try_send_on_pooled_connection(
            conn,
            request,
            original_uri,
            attempt_observer,
        );
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
