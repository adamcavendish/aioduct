use std::net::SocketAddr;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::h2c_probe::{H2cProbeAction, H2cProbeKey, H2cProbeToken};
use crate::observer::ConnectionObservationOwner;
use crate::pool::PooledConnection;
use crate::proxy::ProxyEstablishmentPlan;
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::HttpEngineSend;
use super::connection_deadline::ConnectionDeadline;
use super::h2_peer_settings::H2PeerSettingsRequirement;
use super::proxy_connect::{
    ProxyAttemptError, ProxyConnectCandidates, ProxyConnectionTransitions,
    ProxyEndpointFailureOwner, ProxyHopTransition, ProxyHopTransport, ProxyNegotiation,
    ProxyOriginProtocol, ProxyTargetAttempt, classify_first_proxy_endpoint_error,
    classify_pre_request_endpoint_error, classify_socks4_error, classify_socks5_error,
    configure_proxy_socket,
};
use super::proxy_stream::ProxyStreamSend;

impl<R: RuntimePoll, C: ConnectorSend, O> HttpEngineSend<R, C, O>
where
    O: crate::observer::Observer + Send + Sync + 'static,
    O::ConnectionObservation: Send + Sync,
{
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn connect_via_proxy_plan_adaptive_h2c_send(
        &self,
        h2_plan: &ProxyEstablishmentPlan,
        h1_plan: &ProxyEstablishmentPlan,
        force_addr: Option<SocketAddr>,
        probe_key: &H2cProbeKey,
        attempt_observer: Option<&crate::observer::SendAttemptObserver>,
        deadline: ConnectionDeadline,
    ) -> Result<(PooledConnection<RequestBodySend>, Option<H2cProbeToken>), Error> {
        #[cfg(feature = "rustls")]
        if h2_plan.requires_tls() && self.core.tls.is_none() {
            return Err(Error::Tls("no TLS connector configured".into()));
        }
        #[cfg(feature = "rustls")]
        if let Some(connector) = self.core.tls.as_deref() {
            super::proxy_tls::preflight_https_proxy_hops(connector, h2_plan)?;
            super::proxy_tls::preflight_https_proxy_hops(connector, h1_plan)?;
        }
        #[cfg(not(feature = "rustls"))]
        if h2_plan.requires_tls() || h1_plan.requires_tls() {
            return Err(Error::Tls(
                "HTTPS origins and proxies require the `rustls` TLS backend feature".into(),
            ));
        }

        let mut acquisition = attempt_observer.and_then(|observer| {
            observer.acquisition_scope(crate::observer::ConnectionAcquisitionPhase::Dns)
        });
        let resolved = deadline
            .run::<R, _, _>(ProxyConnectCandidates::resolve(
                &self.core, h2_plan, force_addr,
            ))
            .await;
        if let Some(acquisition) = &mut acquisition {
            acquisition.result(&resolved);
        }
        let candidates = resolved?;
        candidates
            .try_each_target(|first_proxy_addrs, targets| {
                self.connect_via_proxy_attempt_adaptive_h2c_send(
                    h2_plan,
                    h1_plan,
                    first_proxy_addrs,
                    targets,
                    probe_key,
                    attempt_observer,
                    deadline,
                )
            })
            .await
    }

    pub(crate) async fn connect_via_proxy_plan_send(
        &self,
        plan: &ProxyEstablishmentPlan,
        force_addr: Option<SocketAddr>,
        h2_peer_settings: H2PeerSettingsRequirement,
        attempt_observer: Option<&crate::observer::SendAttemptObserver>,
        deadline: ConnectionDeadline,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        #[cfg(feature = "rustls")]
        if plan.requires_tls() && self.core.tls.is_none() {
            return Err(Error::Tls("no TLS connector configured".into()));
        }
        #[cfg(feature = "rustls")]
        if let Some(connector) = self.core.tls.as_deref() {
            super::proxy_tls::preflight_https_proxy_hops(connector, plan)?;
        }
        #[cfg(not(feature = "rustls"))]
        if plan.requires_tls() {
            return Err(Error::Tls(
                "HTTPS origins and proxies require the `rustls` TLS backend feature".into(),
            ));
        }

        let mut acquisition = attempt_observer.and_then(|observer| {
            observer.acquisition_scope(crate::observer::ConnectionAcquisitionPhase::Dns)
        });
        let resolved = deadline
            .run::<R, _, _>(ProxyConnectCandidates::resolve(
                &self.core, plan, force_addr,
            ))
            .await;
        if let Some(acquisition) = &mut acquisition {
            acquisition.result(&resolved);
        }
        let candidates = resolved?;
        candidates
            .try_each_target(|first_proxy_addrs, targets| {
                self.connect_via_proxy_attempt_send(
                    plan,
                    first_proxy_addrs,
                    targets,
                    h2_peer_settings,
                    attempt_observer,
                    deadline,
                )
            })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_via_proxy_attempt_send(
        &self,
        plan: &ProxyEstablishmentPlan,
        first_proxy_addrs: Vec<SocketAddr>,
        targets: ProxyTargetAttempt,
        h2_peer_settings: H2PeerSettingsRequirement,
        attempt_observer: Option<&crate::observer::SendAttemptObserver>,
        deadline: ConnectionDeadline,
    ) -> Result<PooledConnection<RequestBodySend>, ProxyAttemptError> {
        let connection_observation = self.begin_connection_observation();
        let establishment = connection_observation
            .as_ref()
            .and_then(|owner| owner.establishment_scope());
        let mut tcp_acquisition = attempt_observer.and_then(|observer| {
            observer.acquisition_scope(crate::observer::ConnectionAcquisitionPhase::TcpConnect)
        });
        let transport = deadline
            .run::<R, _, _>(
                self.connect_first_proxy_send(&first_proxy_addrs, connection_observation.clone()),
            )
            .await;
        if let Some(acquisition) = &mut tcp_acquisition {
            acquisition.result(&transport);
        }
        if let Some(establishment) = &establishment {
            establishment.result(&transport);
        }
        let (stream, remote_addr) = transport?;
        let mut acquisition = crate::observer::AcquisitionPhases::new(attempt_observer);
        let result = deadline
            .run_proxy::<R, _, _>(self.connect_via_proxy_stream_send(
                stream,
                remote_addr,
                plan,
                targets,
                h2_peer_settings,
                connection_observation,
                &mut acquisition,
            ))
            .await;
        acquisition.finish(result.as_ref().err().map(ProxyAttemptError::error));
        if let (Err(error), Some(establishment)) = (&result, &establishment) {
            establishment.failed(error.error());
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_via_proxy_attempt_adaptive_h2c_send(
        &self,
        h2_plan: &ProxyEstablishmentPlan,
        h1_plan: &ProxyEstablishmentPlan,
        first_proxy_addrs: Vec<SocketAddr>,
        targets: ProxyTargetAttempt,
        probe_key: &H2cProbeKey,
        attempt_observer: Option<&crate::observer::SendAttemptObserver>,
        deadline: ConnectionDeadline,
    ) -> Result<(PooledConnection<RequestBodySend>, Option<H2cProbeToken>), ProxyAttemptError> {
        let connection_observation = self.begin_connection_observation();
        let establishment = connection_observation
            .as_ref()
            .and_then(|owner| owner.establishment_scope());
        let mut tcp_acquisition = attempt_observer.and_then(|observer| {
            observer.acquisition_scope(crate::observer::ConnectionAcquisitionPhase::TcpConnect)
        });
        let transport = deadline
            .run::<R, _, _>(
                self.connect_first_proxy_send(&first_proxy_addrs, connection_observation.clone()),
            )
            .await;
        if let Some(acquisition) = &mut tcp_acquisition {
            acquisition.result(&transport);
        }
        if let Some(establishment) = &establishment {
            establishment.result(&transport);
        }
        let (stream, remote_addr) = transport?;
        let mut acquisition = crate::observer::AcquisitionPhases::new(attempt_observer);
        let endpoint_key = probe_key.proxy_endpoint(
            remote_addr,
            targets.first_target_addr,
            targets.second_target_addr,
        );
        let probe_token = match self.core.h2c_probe_cache.begin_endpoint_probe(endpoint_key) {
            H2cProbeAction::UseH1 => {
                let result = deadline
                    .run_proxy::<R, _, _>(self.connect_via_proxy_stream_send(
                        stream,
                        remote_addr,
                        h1_plan,
                        targets,
                        H2PeerSettingsRequirement::NotRequired,
                        connection_observation,
                        &mut acquisition,
                    ))
                    .await;
                acquisition.finish(result.as_ref().err().map(ProxyAttemptError::error));
                if let (Err(error), Some(establishment)) = (&result, &establishment) {
                    establishment.failed(error.error());
                }
                return result.map(|connection| (connection, None));
            }
            H2cProbeAction::Probe(token) => *token,
        };
        let result = deadline
            .run_proxy::<R, _, _>(self.connect_via_proxy_stream_send(
                stream,
                remote_addr,
                h2_plan,
                targets,
                H2PeerSettingsRequirement::Required,
                connection_observation,
                &mut acquisition,
            ))
            .await;
        acquisition.finish(result.as_ref().err().map(ProxyAttemptError::error));
        if let (Err(error), Some(establishment)) = (&result, &establishment) {
            establishment.failed(error.error());
        }
        match result {
            Ok(connection) => {
                self.core.h2c_probe_cache.confirm_h2c_endpoint(probe_token);
                Ok((connection, None))
            }
            Err(ProxyAttemptError::AdaptiveH2cRejected(_)) => {
                self.core.h2c_probe_cache.reject_h2c_endpoint(&probe_token);
                let fallback_observation = self.begin_connection_observation();
                let fallback_establishment = fallback_observation
                    .as_ref()
                    .and_then(|owner| owner.establishment_scope());
                let mut tcp_acquisition = attempt_observer.and_then(|observer| {
                    observer
                        .acquisition_scope(crate::observer::ConnectionAcquisitionPhase::TcpConnect)
                });
                let transport = deadline
                    .run::<R, _, _>(
                        self.connect_first_proxy_send(&[remote_addr], fallback_observation.clone()),
                    )
                    .await;
                if let Some(acquisition) = &mut tcp_acquisition {
                    acquisition.result(&transport);
                }
                if let Some(establishment) = &fallback_establishment {
                    establishment.result(&transport);
                }
                let (stream, fallback_addr) = transport
                    .map_err(|error| classify_first_proxy_endpoint_error(remote_addr, error))?;
                debug_assert_eq!(fallback_addr, remote_addr);
                let result = deadline
                    .run_proxy::<R, _, _>(self.connect_via_proxy_stream_send(
                        stream,
                        fallback_addr,
                        h1_plan,
                        targets,
                        H2PeerSettingsRequirement::NotRequired,
                        fallback_observation,
                        &mut acquisition,
                    ))
                    .await;
                acquisition.finish(result.as_ref().err().map(ProxyAttemptError::error));
                if let (Err(error), Some(establishment)) = (&result, &fallback_establishment) {
                    establishment.failed(error.error());
                }
                result.map(|connection| (connection, Some(probe_token)))
            }
            Err(error) => Err(error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_via_proxy_stream_send(
        &self,
        stream: C::Stream,
        remote_addr: SocketAddr,
        plan: &ProxyEstablishmentPlan,
        targets: ProxyTargetAttempt,
        h2_peer_settings: H2PeerSettingsRequirement,
        connection_observation: Option<ConnectionObservationOwner>,
        acquisition: &mut crate::observer::AcquisitionPhases<
            '_,
            crate::observer::SendAttemptObserver,
        >,
    ) -> Result<PooledConnection<RequestBodySend>, ProxyAttemptError> {
        let mut connection = match ProxyHopTransport::for_hop(plan.first()) {
            ProxyHopTransport::Tls {
                server_name: _server_name,
            } => {
                #[cfg(feature = "rustls")]
                {
                    let connector = self
                        .core
                        .tls
                        .as_deref()
                        .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;
                    if let Some(observation) = connection_observation.as_ref()
                        && observation
                            .interest()
                            .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
                    {
                        observation.emit(
                            &crate::observer::ConnectionObservationEvent::PhaseStarted {
                                phase: crate::observer::ConnectionEstablishmentPhase::TlsHandshake,
                                at: crate::observer::Instant::now(),
                            },
                        );
                    }
                    acquisition.start(crate::observer::ConnectionAcquisitionPhase::TlsHandshake);
                    let stream = super::proxy_tls::connect_send(
                        connector,
                        _server_name,
                        ProxyStreamSend::new(stream),
                    )
                    .await
                    .map_err(|error| {
                        let error = classify_first_proxy_endpoint_error(remote_addr, error);
                        if let Some(connection_observation) = connection_observation.as_ref() {
                            connection_observation.establishment_failed(error.error());
                        }
                        error
                    })?;
                    acquisition.complete();
                    if let Some(observation) = connection_observation.as_ref()
                        && observation
                            .interest()
                            .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
                    {
                        observation.emit(
                            &crate::observer::ConnectionObservationEvent::PhaseCompleted {
                                phase: crate::observer::ConnectionEstablishmentPhase::TlsHandshake,
                                at: crate::observer::Instant::now(),
                            },
                        );
                    }
                    self.connect_after_first_proxy_send(
                        ProxyStreamSend::new(stream),
                        remote_addr,
                        plan,
                        targets,
                        h2_peer_settings,
                        connection_observation,
                        acquisition,
                    )
                    .await?
                }
                #[cfg(not(feature = "rustls"))]
                unreachable!()
            }
            ProxyHopTransport::Plain => {
                self.connect_after_first_proxy_send(
                    ProxyStreamSend::new(stream),
                    remote_addr,
                    plan,
                    targets,
                    h2_peer_settings,
                    connection_observation,
                    acquisition,
                )
                .await?
            }
        };
        connection.remote_addr = Some(remote_addr);
        Ok(connection)
    }

    async fn connect_first_proxy_send(
        &self,
        addrs: &[SocketAddr],
        observation: Option<ConnectionObservationOwner>,
    ) -> Result<(C::Stream, SocketAddr), Error> {
        if let Some(observation) = observation.as_ref()
            && observation
                .interest()
                .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
        {
            observation.emit(&crate::observer::ConnectionObservationEvent::PhaseStarted {
                phase: crate::observer::ConnectionEstablishmentPhase::TcpConnect,
                at: crate::observer::Instant::now(),
            });
        }
        let connect = if let Some(observation) = observation.as_ref()
            && observation
                .interest()
                .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
        {
            crate::happy_eyeballs::connect_happy_eyeballs_observed::<R, C, _>(
                &self.connector,
                addrs,
                self.core.local_address,
                observation,
            )
            .await
        } else {
            crate::happy_eyeballs::connect_happy_eyeballs::<R, C>(
                &self.connector,
                addrs,
                self.core.local_address,
            )
            .await
            .map_err(Error::Io)
        };
        let (stream, remote_addr) = connect.inspect_err(|error| {
            if let Some(observation) = observation.as_ref() {
                observation.establishment_failed(error);
            }
        })?;

        if let Some(observation) = observation.as_ref()
            && observation
                .interest()
                .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
        {
            observation.emit(
                &crate::observer::ConnectionObservationEvent::PhaseCompleted {
                    phase: crate::observer::ConnectionEstablishmentPhase::TcpConnect,
                    at: crate::observer::Instant::now(),
                },
            );
        }

        if let Err(error) = configure_proxy_socket(&self.core, &stream) {
            if let Some(observation) = observation.as_ref() {
                observation.establishment_failed(&error);
            }
            return Err(error);
        }

        Ok((stream, remote_addr))
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_after_first_proxy_send(
        &self,
        stream: ProxyStreamSend,
        first_proxy_addr: SocketAddr,
        plan: &ProxyEstablishmentPlan,
        targets: ProxyTargetAttempt,
        h2_peer_settings: H2PeerSettingsRequirement,
        connection_observation: Option<ConnectionObservationOwner>,
        acquisition: &mut crate::observer::AcquisitionPhases<
            '_,
            crate::observer::SendAttemptObserver,
        >,
    ) -> Result<PooledConnection<RequestBodySend>, ProxyAttemptError> {
        let transitions = ProxyConnectionTransitions::new(
            plan,
            targets.first_target_addr,
            targets.second_target_addr,
        );
        let first = transitions.first();
        let stream = self
            .negotiate_proxy_send(
                stream,
                first.negotiation()?,
                Some(ProxyEndpointFailureOwner::FirstProxy(first_proxy_addr)),
                connection_observation.clone(),
                acquisition,
            )
            .await?;
        let Some(second) = transitions.second() else {
            return self
                .connect_proxy_origin_send(
                    stream,
                    plan,
                    h2_peer_settings,
                    connection_observation,
                    acquisition,
                )
                .await
                .map_err(|error| {
                    if h2_peer_settings.is_required() {
                        first.classify_adaptive_h2c_setup_error(error)
                    } else {
                        first.classify_target_setup_error(error)
                    }
                });
        };

        match second.transport() {
            ProxyHopTransport::Tls {
                server_name: _server_name,
            } => {
                #[cfg(feature = "rustls")]
                {
                    let connector = self
                        .core
                        .tls
                        .as_deref()
                        .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;
                    if let Some(observation) = connection_observation.as_ref()
                        && observation
                            .interest()
                            .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
                    {
                        observation.emit(
                            &crate::observer::ConnectionObservationEvent::PhaseStarted {
                                phase: crate::observer::ConnectionEstablishmentPhase::TlsHandshake,
                                at: crate::observer::Instant::now(),
                            },
                        );
                    }
                    acquisition.start(crate::observer::ConnectionAcquisitionPhase::TlsHandshake);
                    let stream = super::proxy_tls::connect_send(connector, _server_name, stream)
                        .await
                        .map_err(|error| {
                            let error = first.classify_target_setup_error(error);
                            if let Some(connection_observation) = connection_observation.as_ref() {
                                connection_observation.establishment_failed(error.error());
                            }
                            error
                        })?;
                    acquisition.complete();
                    if let Some(observation) = connection_observation.as_ref()
                        && observation
                            .interest()
                            .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
                    {
                        observation.emit(
                            &crate::observer::ConnectionObservationEvent::PhaseCompleted {
                                phase: crate::observer::ConnectionEstablishmentPhase::TlsHandshake,
                                at: crate::observer::Instant::now(),
                            },
                        );
                    }
                    self.connect_final_proxy_send(
                        ProxyStreamSend::new(stream),
                        second,
                        first
                            .local_target_hop()
                            .map(ProxyEndpointFailureOwner::LocalTarget),
                        plan,
                        h2_peer_settings,
                        connection_observation,
                        acquisition,
                    )
                    .await
                }
                #[cfg(not(feature = "rustls"))]
                unreachable!()
            }
            ProxyHopTransport::Plain => {
                self.connect_final_proxy_send(
                    stream,
                    second,
                    first
                        .local_target_hop()
                        .map(ProxyEndpointFailureOwner::LocalTarget),
                    plan,
                    h2_peer_settings,
                    connection_observation,
                    acquisition,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_final_proxy_send(
        &self,
        stream: ProxyStreamSend,
        transition: ProxyHopTransition<'_>,
        endpoint_failure_owner: Option<ProxyEndpointFailureOwner>,
        plan: &ProxyEstablishmentPlan,
        h2_peer_settings: H2PeerSettingsRequirement,
        connection_observation: Option<ConnectionObservationOwner>,
        acquisition: &mut crate::observer::AcquisitionPhases<
            '_,
            crate::observer::SendAttemptObserver,
        >,
    ) -> Result<PooledConnection<RequestBodySend>, ProxyAttemptError> {
        let stream = self
            .negotiate_proxy_send(
                stream,
                transition.negotiation()?,
                endpoint_failure_owner,
                connection_observation.clone(),
                acquisition,
            )
            .await?;
        self.connect_proxy_origin_send(
            stream,
            plan,
            h2_peer_settings,
            connection_observation,
            acquisition,
        )
        .await
        .map_err(|error| {
            if h2_peer_settings.is_required() {
                transition.classify_adaptive_h2c_setup_error(error)
            } else {
                transition.classify_target_setup_error(error)
            }
        })
    }

    async fn negotiate_proxy_send(
        &self,
        mut stream: ProxyStreamSend,
        negotiation: ProxyNegotiation<'_>,
        endpoint_failure_owner: Option<ProxyEndpointFailureOwner>,
        connection_observation: Option<ConnectionObservationOwner>,
        acquisition: &mut crate::observer::AcquisitionPhases<
            '_,
            crate::observer::SendAttemptObserver,
        >,
    ) -> Result<ProxyStreamSend, ProxyAttemptError> {
        acquisition.start(crate::observer::ConnectionAcquisitionPhase::ProxyConnect);
        if let Some(observation) = connection_observation.as_ref()
            && observation
                .interest()
                .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
        {
            observation.emit(&crate::observer::ConnectionObservationEvent::PhaseStarted {
                phase: crate::observer::ConnectionEstablishmentPhase::ProxyConnect,
                at: crate::observer::Instant::now(),
            });
        }
        let result = match negotiation {
            ProxyNegotiation::HttpConnect {
                proxy,
                connect_target,
            } => super::connect_handshake::do_connect_handshake(
                stream,
                proxy,
                connect_target.as_ref(),
            )
            .await
            .map_err(|error| classify_pre_request_endpoint_error(endpoint_failure_owner, error)),
            ProxyNegotiation::Socks4Local {
                address,
                auth,
                fallback_hop,
            } => {
                crate::socks4::socks4_handshake_async(
                    &mut stream,
                    *address.ip(),
                    address.port(),
                    auth,
                )
                .await
                .map_err(|error| {
                    classify_socks4_error(Some(fallback_hop), endpoint_failure_owner, error)
                })?;
                Ok(stream)
            }
            ProxyNegotiation::Socks4aRemote { host, port, auth } => {
                crate::socks4::socks4a_handshake_async(&mut stream, host, port, auth)
                    .await
                    .map_err(|error| classify_socks4_error(None, endpoint_failure_owner, error))?;
                Ok(stream)
            }
            ProxyNegotiation::Socks5 {
                host,
                port,
                auth,
                dns,
                resolved_addr,
                fallback_hop,
            } => {
                crate::socks5::socks5_handshake_async(
                    &mut stream,
                    host,
                    port,
                    auth,
                    dns,
                    resolved_addr,
                )
                .await
                .map_err(|error| {
                    classify_socks5_error(fallback_hop, endpoint_failure_owner, error)
                })?;
                Ok(stream)
            }
        };
        match result {
            Ok(stream) => {
                acquisition.complete();
                if let Some(observation) = connection_observation.as_ref()
                    && observation
                        .interest()
                        .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
                {
                    observation.emit(
                        &crate::observer::ConnectionObservationEvent::PhaseCompleted {
                            phase: crate::observer::ConnectionEstablishmentPhase::ProxyConnect,
                            at: crate::observer::Instant::now(),
                        },
                    );
                }
                Ok(stream)
            }
            Err(error) => {
                if let Some(observation) = connection_observation.as_ref() {
                    observation.establishment_failed(error.error());
                }
                Err(error)
            }
        }
    }

    async fn connect_proxy_origin_send(
        &self,
        stream: ProxyStreamSend,
        plan: &ProxyEstablishmentPlan,
        h2_peer_settings: H2PeerSettingsRequirement,
        connection_observation: Option<ConnectionObservationOwner>,
        acquisition: &mut crate::observer::AcquisitionPhases<
            '_,
            crate::observer::SendAttemptObserver,
        >,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        match ProxyOriginProtocol::for_plan(plan)? {
            ProxyOriginProtocol::Tls {
                server_name: _server_name,
                protocol_hint: _protocol_hint,
            } => {
                acquisition.start(crate::observer::ConnectionAcquisitionPhase::TlsHandshake);
                #[cfg(feature = "rustls")]
                return self
                    .connect_tls_with_hint_observed_with_owner(
                        stream,
                        _server_name,
                        _protocol_hint,
                        connection_observation,
                        |_, _| acquisition.complete(),
                    )
                    .await;
                #[cfg(not(feature = "rustls"))]
                unreachable!();
            }
            ProxyOriginProtocol::Http1 => {
                self.connect_h1_with_observation(stream, connection_observation)
                    .await
            }
            ProxyOriginProtocol::Http2 if h2_peer_settings.is_required() => {
                self.connect_h2_prior_knowledge_confirmed_with_observation(
                    stream,
                    connection_observation,
                )
                .await
            }
            ProxyOriginProtocol::Http2 => {
                self.connect_h2_prior_knowledge_with_observation(stream, connection_observation)
                    .await
            }
        }
    }
}
