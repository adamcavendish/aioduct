use std::net::SocketAddr;

use http::{Method, Uri};

use crate::body::RequestBodySend;
use crate::clock::Instant;
use crate::error::Error;
use crate::h2c_probe::{H2cProbeAction, H2cProbeKey, H2cProbeToken};
use crate::pool::PooledConnection;
use crate::proxy::ProxyEstablishmentPlan;
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::HttpEngineSend;
use super::h2_peer_settings::H2PeerSettingsRequirement;
use super::proxy_connect::{
    ProxyAttemptError, ProxyAttemptObservation, ProxyConnectCandidates, ProxyConnectionTransitions,
    ProxyEndpointFailureOwner, ProxyHopTransition, ProxyHopTransport, ProxyNegotiation,
    ProxyOriginProtocol, ProxyTargetAttempt, classify_first_proxy_endpoint_error,
    classify_pre_request_endpoint_error, classify_socks4_error, classify_socks5_error,
    configure_proxy_socket,
};
use super::proxy_stream::ProxyStreamSend;

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineSend<R, C> {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn connect_via_proxy_plan_adaptive_h2c_send(
        &self,
        h2_plan: &ProxyEstablishmentPlan,
        h1_plan: &ProxyEstablishmentPlan,
        method: &Method,
        uri: &Uri,
        force_addr: Option<SocketAddr>,
        probe_key: &H2cProbeKey,
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

        let candidates =
            ProxyConnectCandidates::resolve(&self.core, h2_plan, method, uri, force_addr).await?;
        candidates
            .try_each_target(|first_proxy_addrs, targets| {
                self.connect_via_proxy_attempt_adaptive_h2c_send(
                    h2_plan,
                    h1_plan,
                    first_proxy_addrs,
                    targets,
                    method,
                    uri,
                    probe_key,
                )
            })
            .await
    }

    pub(crate) async fn connect_via_proxy_plan_send(
        &self,
        plan: &ProxyEstablishmentPlan,
        method: &Method,
        uri: &Uri,
        force_addr: Option<SocketAddr>,
        h2_peer_settings: H2PeerSettingsRequirement,
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

        let candidates =
            ProxyConnectCandidates::resolve(&self.core, plan, method, uri, force_addr).await?;
        candidates
            .try_each_target(|first_proxy_addrs, targets| {
                self.connect_via_proxy_attempt_send(
                    plan,
                    first_proxy_addrs,
                    targets,
                    method,
                    uri,
                    h2_peer_settings,
                )
            })
            .await
    }

    async fn connect_via_proxy_attempt_send(
        &self,
        plan: &ProxyEstablishmentPlan,
        first_proxy_addrs: Vec<SocketAddr>,
        targets: ProxyTargetAttempt,
        method: &Method,
        uri: &Uri,
        h2_peer_settings: H2PeerSettingsRequirement,
    ) -> Result<PooledConnection<RequestBodySend>, ProxyAttemptError> {
        let tcp_start = Instant::now();
        let (stream, remote_addr) = self.connect_first_proxy_send(&first_proxy_addrs).await?;
        self.connect_via_proxy_stream_send(
            stream,
            remote_addr,
            tcp_start,
            plan,
            targets,
            method,
            uri,
            h2_peer_settings,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_via_proxy_attempt_adaptive_h2c_send(
        &self,
        h2_plan: &ProxyEstablishmentPlan,
        h1_plan: &ProxyEstablishmentPlan,
        first_proxy_addrs: Vec<SocketAddr>,
        targets: ProxyTargetAttempt,
        method: &Method,
        uri: &Uri,
        probe_key: &H2cProbeKey,
    ) -> Result<(PooledConnection<RequestBodySend>, Option<H2cProbeToken>), ProxyAttemptError> {
        let tcp_start = Instant::now();
        let (stream, remote_addr) = self.connect_first_proxy_send(&first_proxy_addrs).await?;
        let endpoint_key = probe_key.proxy_endpoint(
            remote_addr,
            targets.first_target_addr,
            targets.second_target_addr,
        );
        let probe_token = match self.core.h2c_probe_cache.begin_endpoint_probe(endpoint_key) {
            H2cProbeAction::UseH1 => {
                let connection = self
                    .connect_via_proxy_stream_send(
                        stream,
                        remote_addr,
                        tcp_start,
                        h1_plan,
                        targets,
                        method,
                        uri,
                        H2PeerSettingsRequirement::NotRequired,
                    )
                    .await?;
                return Ok((connection, None));
            }
            H2cProbeAction::Probe(token) => *token,
        };

        match self
            .connect_via_proxy_stream_send(
                stream,
                remote_addr,
                tcp_start,
                h2_plan,
                targets,
                method,
                uri,
                H2PeerSettingsRequirement::Required,
            )
            .await
        {
            Ok(connection) => {
                self.core.h2c_probe_cache.confirm_h2c_endpoint(probe_token);
                Ok((connection, None))
            }
            Err(ProxyAttemptError::LocalTarget { hop, source }) => {
                Err(ProxyAttemptError::LocalTarget { hop, source })
            }
            Err(error @ ProxyAttemptError::FirstProxyEndpoint { .. }) => Err(error),
            Err(ProxyAttemptError::Fatal(error)) => Err(ProxyAttemptError::Fatal(error)),
            Err(ProxyAttemptError::AdaptiveH2cRejected(_)) => {
                self.core.h2c_probe_cache.reject_h2c_endpoint(&probe_token);
                let fallback_start = Instant::now();
                let (stream, fallback_addr) =
                    self.connect_first_proxy_send(&[remote_addr])
                        .await
                        .map_err(|error| classify_first_proxy_endpoint_error(remote_addr, error))?;
                debug_assert_eq!(fallback_addr, remote_addr);
                let connection = self
                    .connect_via_proxy_stream_send(
                        stream,
                        fallback_addr,
                        fallback_start,
                        h1_plan,
                        targets,
                        method,
                        uri,
                        H2PeerSettingsRequirement::NotRequired,
                    )
                    .await?;
                Ok((connection, Some(probe_token)))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_via_proxy_stream_send(
        &self,
        stream: C::Stream,
        remote_addr: SocketAddr,
        tcp_start: Instant,
        plan: &ProxyEstablishmentPlan,
        targets: ProxyTargetAttempt,
        method: &Method,
        uri: &Uri,
        h2_peer_settings: H2PeerSettingsRequirement,
    ) -> Result<PooledConnection<RequestBodySend>, ProxyAttemptError> {
        let mut observation = ProxyAttemptObservation::new(
            &self.core,
            method,
            uri,
            plan.first().scheme(),
            plan.protocol_hint(),
            remote_addr,
            tcp_start.elapsed(),
        );
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
                    let tls_start = Instant::now();
                    let stream = super::proxy_tls::connect_send(
                        connector,
                        _server_name,
                        ProxyStreamSend::new(stream),
                    )
                    .await
                    .map_err(|error| classify_first_proxy_endpoint_error(remote_addr, error))?;
                    observation.record_proxy_tls(&stream, tls_start.elapsed());
                    self.connect_after_first_proxy_send(
                        ProxyStreamSend::new(stream),
                        remote_addr,
                        plan,
                        targets,
                        &mut observation,
                        h2_peer_settings,
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
                    &mut observation,
                    h2_peer_settings,
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
    ) -> Result<(C::Stream, SocketAddr), Error> {
        let (stream, remote_addr) = crate::happy_eyeballs::connect_happy_eyeballs::<R, C>(
            &self.connector,
            addrs,
            self.core.local_address,
        )
        .await
        .map_err(Error::Io)?;

        configure_proxy_socket(&self.core, &stream)?;

        Ok((stream, remote_addr))
    }

    async fn connect_after_first_proxy_send(
        &self,
        stream: ProxyStreamSend,
        first_proxy_addr: SocketAddr,
        plan: &ProxyEstablishmentPlan,
        targets: ProxyTargetAttempt,
        observation: &mut ProxyAttemptObservation,
        h2_peer_settings: H2PeerSettingsRequirement,
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
            )
            .await?;
        let Some(second) = transitions.second() else {
            return self
                .connect_proxy_origin_send(stream, plan, observation, h2_peer_settings)
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
                    let tls_start = Instant::now();
                    let stream = super::proxy_tls::connect_send(connector, _server_name, stream)
                        .await
                        .map_err(|error| first.classify_target_setup_error(error))?;
                    observation.record_proxy_tls(&stream, tls_start.elapsed());
                    self.connect_final_proxy_send(
                        ProxyStreamSend::new(stream),
                        second,
                        first
                            .local_target_hop()
                            .map(ProxyEndpointFailureOwner::LocalTarget),
                        plan,
                        observation,
                        h2_peer_settings,
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
                    observation,
                    h2_peer_settings,
                )
                .await
            }
        }
    }

    async fn connect_final_proxy_send(
        &self,
        stream: ProxyStreamSend,
        transition: ProxyHopTransition<'_>,
        endpoint_failure_owner: Option<ProxyEndpointFailureOwner>,
        plan: &ProxyEstablishmentPlan,
        observation: &mut ProxyAttemptObservation,
        h2_peer_settings: H2PeerSettingsRequirement,
    ) -> Result<PooledConnection<RequestBodySend>, ProxyAttemptError> {
        let stream = self
            .negotiate_proxy_send(stream, transition.negotiation()?, endpoint_failure_owner)
            .await?;
        self.connect_proxy_origin_send(stream, plan, observation, h2_peer_settings)
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
    ) -> Result<ProxyStreamSend, ProxyAttemptError> {
        match negotiation {
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
        }
    }

    async fn connect_proxy_origin_send(
        &self,
        stream: ProxyStreamSend,
        plan: &ProxyEstablishmentPlan,
        _observation: &mut ProxyAttemptObservation,
        h2_peer_settings: H2PeerSettingsRequirement,
    ) -> Result<PooledConnection<RequestBodySend>, Error> {
        match ProxyOriginProtocol::for_plan(plan)? {
            ProxyOriginProtocol::Tls {
                server_name: _server_name,
                protocol_hint: _protocol_hint,
            } => {
                #[cfg(feature = "rustls")]
                return self
                    .connect_tls_with_hint_observed(
                        stream,
                        _server_name,
                        _protocol_hint,
                        |stream, duration| _observation.record_proxy_tls(stream, duration),
                    )
                    .await;
                #[cfg(not(feature = "rustls"))]
                unreachable!();
            }
            ProxyOriginProtocol::Http1 => self.connect_h1(stream).await,
            ProxyOriginProtocol::Http2 if h2_peer_settings.is_required() => {
                let (connection, confirmation) =
                    self.connect_h2_prior_knowledge_confirmed(stream).await?;
                if confirmation.confirmed_within::<R>().await {
                    Ok(connection)
                } else {
                    Err(Error::Other(
                        "proxy tunnel peer did not send an HTTP/2 SETTINGS preface".into(),
                    ))
                }
            }
            ProxyOriginProtocol::Http2 => self.connect_h2_prior_knowledge(stream).await,
        }
    }
}
