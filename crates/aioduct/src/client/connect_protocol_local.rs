use std::future::Future;
use std::pin::Pin;

use crate::body::RequestBodyLocal;
use crate::error::Error;
use crate::observer::LocalConnectionObservationOwner;
use crate::pool::{PooledConnection, ProtocolHint};
use crate::runtime::{ConnectorLocal, RuntimeLocal};

use super::HttpEngineLocal;

impl<R: RuntimeLocal, C: ConnectorLocal + Clone, O> HttpEngineLocal<R, C, O>
where
    O: crate::observer::Observer + 'static,
{
    pub(crate) fn begin_connection_observation(&self) -> Option<LocalConnectionObservationOwner>
    where
        O: crate::observer::Observer,
    {
        LocalConnectionObservationOwner::local(&*self.observer)
    }

    #[allow(dead_code)]
    pub(crate) fn connect_plaintext_local_with_hint<S>(
        &self,
        stream: S,
        force_h2c: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodyLocal>, Error>> + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_plaintext_local_with_hint_observed(
            stream,
            force_h2c,
            self.begin_connection_observation(),
        )
    }

    pub(crate) fn connect_plaintext_local_with_hint_observed<S>(
        &self,
        stream: S,
        force_h2c: bool,
        observation: Option<LocalConnectionObservationOwner>,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodyLocal>, Error>> + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        if force_h2c {
            Box::pin(self.connect_h2_prior_knowledge_local_with_observation(stream, observation))
        } else {
            Box::pin(self.connect_h1_local_with_observation(stream, observation))
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn connect_h1_local<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_h1_local_with_observation(stream, self.begin_connection_observation())
            .await
    }

    pub(crate) async fn connect_h1_local_with_observation<S>(
        &self,
        stream: S,
        observation: Option<LocalConnectionObservationOwner>,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let transport = observation
            .as_ref()
            .and_then(crate::observer::transport::LocalTransportObservation::new);
        if let Some(transport) = transport {
            let stream = crate::observer::transport::ObservedIo::new(stream, transport.clone());
            self.handshake_h1_local(stream, observation, Some(transport))
                .await
        } else {
            self.handshake_h1_local(stream, observation, None).await
        }
    }

    async fn handshake_h1_local<S>(
        &self,
        stream: S,
        observation: Option<LocalConnectionObservationOwner>,
        transport: Option<crate::observer::transport::LocalTransportObservation>,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let (sender, conn) = match hyper::client::conn::http1::handshake(stream).await {
            Ok(value) => value,
            Err(error) => {
                let error = crate::error::Error::from(error);
                if let Some(observation) = observation.as_ref() {
                    observation.establishment_failed(&error);
                }
                return Err(error);
            }
        };

        let handle = crate::upgrade::UpgradeHandleLocal::new();
        let handle_clone = handle.clone();
        let mut pooled = PooledConnection::new_h1(sender);
        if let Some(observation) = observation {
            let connection = crate::observer::ConnectionAssociation::allocate();
            pooled
                .metrics
                .set_local_observation(connection, observation);
            pooled.observe_established();
        }
        if let Some(transport) = &transport {
            transport.established();
        }

        R::spawn_local(async move {
            match conn.without_shutdown().await {
                Ok(parts) => {
                    if let Some(transport) = &transport {
                        transport.driver_ended(None);
                    }
                    let upgraded = crate::upgrade::UpgradedLocal::new(parts.io, parts.read_buf);
                    handle_clone.fulfill(upgraded);
                }
                Err(error) => {
                    if let Some(transport) = transport {
                        transport.driver_ended(Some(crate::error::Error::from(error)));
                    }
                    handle_clone.fail();
                }
            }
        });

        pooled.upgrade_handle_local = Some(handle);
        Ok(pooled)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn connect_h2_prior_knowledge_local<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_h2_prior_knowledge_local_with_observation(
            stream,
            self.begin_connection_observation(),
        )
        .await
    }

    pub(crate) async fn connect_h2_prior_knowledge_local_with_observation<S>(
        &self,
        stream: S,
        observation: Option<LocalConnectionObservationOwner>,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.handshake_h2_local(stream, observation, None).await
    }

    async fn handshake_h2_local<S>(
        &self,
        stream: S,
        observation: Option<LocalConnectionObservationOwner>,
        confirmation: Option<super::h2_peer_settings::H2PeerSettingsConfirmation>,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let mut builder = hyper::client::conn::http2::Builder::new(
            crate::runtime::executor::completion_executor::<R>(),
        );
        if let Some(ref h2) = self.core.http2 {
            h2.apply(&mut builder);
        }
        let (sender, mut conn) = match builder.handshake(stream).await {
            Ok(value) => value,
            Err(error) => {
                let error = crate::error::Error::from(error);
                if let Some(observation) = observation.as_ref() {
                    observation.establishment_failed(&error);
                }
                return Err(error);
            }
        };
        if let Some(confirmation) = confirmation {
            // Poll the driver locally until the policy-required SETTINGS preface
            // arrives. No association or lifecycle is published before acceptance.
            let result = confirmation.accept_with_driver::<R, _>(&mut conn).await;
            if let Err(error) = result {
                if let Some(observation) = observation.as_ref() {
                    observation.establishment_failed(&error);
                }
                return Err(error);
            }
        }

        let mut pooled = PooledConnection::new_h2(sender);
        if let Some(observation) = observation {
            let connection = crate::observer::ConnectionAssociation::allocate();
            pooled
                .metrics
                .set_local_observation(connection, observation);
            pooled.observe_established();
        }
        let metrics = std::sync::Arc::clone(&pooled.metrics);
        R::spawn_local(async move {
            let result = conn.await.map_err(crate::error::Error::from);
            metrics.observe_closed(result.err());
        });
        Ok(pooled)
    }

    pub(crate) async fn connect_h2_prior_knowledge_local_confirmed_with_observation<S>(
        &self,
        stream: S,
        observation: Option<LocalConnectionObservationOwner>,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        let receive_max_frame_size = self
            .core
            .http2
            .as_ref()
            .and_then(|config| config.max_frame_size)
            .map_or(crate::http2::Http2Config::DEFAULT_MAX_FRAME_SIZE, |size| {
                size as usize
            });
        let (stream, confirmation) =
            super::h2_peer_settings::observe_h2_peer_settings(stream, receive_max_frame_size);
        self.handshake_h2_local(stream, observation, Some(confirmation))
            .await
    }

    #[cfg(all(feature = "rustls", feature = "compio"))]
    #[cfg(test)]
    pub(crate) async fn connect_tls_local<S>(
        &self,
        tcp_stream: S,
        host: &str,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_tls_local_with_hint(tcp_stream, host, ProtocolHint::Auto)
            .await
    }

    #[cfg(all(feature = "rustls", feature = "compio"))]
    #[allow(dead_code)]
    pub(crate) async fn connect_tls_local_with_hint<S>(
        &self,
        tcp_stream: S,
        host: &str,
        protocol_hint: ProtocolHint,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_tls_local_with_hint_observed(tcp_stream, host, protocol_hint, |_, _| {})
            .await
    }

    #[cfg(all(feature = "rustls", feature = "compio"))]
    pub(crate) async fn connect_tls_local_with_hint_observed<S, F>(
        &self,
        tcp_stream: S,
        host: &str,
        protocol_hint: ProtocolHint,
        on_tls_complete: F,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        F: FnOnce(&crate::tls::TlsStream<S>, std::time::Duration),
    {
        self.connect_tls_local_with_hint_observed_with_owner(
            tcp_stream,
            host,
            protocol_hint,
            self.begin_connection_observation(),
            on_tls_complete,
        )
        .await
    }

    #[cfg(all(feature = "rustls", feature = "compio"))]
    pub(crate) async fn connect_tls_local_with_hint_observed_with_owner<S, F>(
        &self,
        tcp_stream: S,
        host: &str,
        protocol_hint: ProtocolHint,
        observation: Option<LocalConnectionObservationOwner>,
        on_tls_complete: F,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        F: FnOnce(&crate::tls::TlsStream<S>, std::time::Duration),
    {
        if let Some(observation) = observation.as_ref()
            && observation
                .interest()
                .contains(crate::observer::Interest::CONNECTION_ESTABLISHMENT)
        {
            observation.emit(&crate::observer::ConnectionObservationEvent::PhaseStarted {
                phase: crate::observer::ConnectionEstablishmentPhase::TlsHandshake,
                at: crate::observer::Instant::now(),
            });
        }

        use crate::tls::TlsConnectLocal;
        use std::time::Instant;

        let tls_start = Instant::now();

        let mut tls_connector = self
            .core
            .tls
            .as_deref()
            .ok_or_else(|| {
                let error = Error::Tls("no TLS connector configured".into());
                if let Some(observation) = observation.as_ref() {
                    observation.establishment_failed(&error);
                }
                error
            })?
            .clone();
        match protocol_hint {
            ProtocolHint::Http1 => {
                tls_connector.config_mut().alpn_protocols = vec![b"http/1.1".to_vec()];
            }
            ProtocolHint::Http2 | ProtocolHint::H2c => {
                tls_connector.config_mut().alpn_protocols = vec![b"h2".to_vec()];
            }
            ProtocolHint::Auto | ProtocolHint::Http3 | ProtocolHint::AdaptiveH2c => {}
        }

        let tls_stream = <crate::tls::RustlsConnector as TlsConnectLocal<S>>::connect_local(
            &tls_connector,
            host,
            tcp_stream,
        )
        .await
        .map_err(|e| {
            let error = Error::Tls(Box::new(e));
            if let Some(observation) = observation.as_ref() {
                observation.establishment_failed(&error);
            }
            error
        })?;

        let tls_duration = tls_start.elapsed();
        on_tls_complete(&tls_stream, tls_duration);

        let alpn =
            crate::tls::RustlsConnector::negotiated_http_protocol(tls_stream.tls_connection())
                .map_err(|protocol| {
                    let error = Error::Unsupported(format!(
                        "upstream negotiated unsupported ALPN protocol `{}`",
                        String::from_utf8_lossy(protocol)
                    ));
                    if let Some(observation) = observation.as_ref() {
                        observation.establishment_failed(&error);
                    }
                    error
                })?;
        let tls_info = tls_stream.tls_info();

        if matches!(protocol_hint, ProtocolHint::Http2 | ProtocolHint::H2c)
            && alpn != Some(crate::tls::AlpnProtocol::H2)
        {
            let error =
                Error::Unsupported("upstream did not negotiate required HTTP/2 ALPN".to_owned());
            if let Some(observation) = observation.as_ref() {
                observation.establishment_failed(&error);
            }
            return Err(error);
        }

        if let Some(observation) = observation.as_ref()
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

        match alpn {
            Some(crate::tls::AlpnProtocol::H2) => {
                let mut builder = hyper::client::conn::http2::Builder::new(
                    crate::runtime::executor::completion_executor::<R>(),
                );
                if let Some(ref h2) = self.core.http2 {
                    h2.apply(&mut builder);
                }
                let (sender, conn) = builder.handshake(tls_stream).await.map_err(|error| {
                    let error = Error::from(error);
                    if let Some(observation) = observation.as_ref() {
                        observation.establishment_failed(&error);
                    }
                    error
                })?;
                let mut pooled = PooledConnection::new_h2(sender);
                if let Some(observation) = observation {
                    let connection = crate::observer::ConnectionAssociation::allocate();
                    pooled
                        .metrics
                        .set_local_observation(connection, observation);
                    pooled.observe_established();
                }
                let metrics = std::sync::Arc::clone(&pooled.metrics);
                R::spawn_local(async move {
                    let result = conn.await.map_err(crate::error::Error::from);
                    metrics.observe_closed(result.err());
                });
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
            _ => {
                let mut pooled = self
                    .connect_h1_local_with_observation(tls_stream, observation)
                    .await?;
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
        }
    }

    #[cfg(all(feature = "rustls", not(feature = "compio")))]
    #[allow(dead_code)]
    pub(crate) async fn connect_tls_local_with_hint<S>(
        &self,
        _tcp_stream: S,
        _host: &str,
        _protocol_hint: ProtocolHint,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_tls_local_with_hint_observed_with_owner(
            _tcp_stream,
            _host,
            _protocol_hint,
            self.begin_connection_observation(),
            |_, _| {},
        )
        .await
    }

    #[cfg(all(feature = "rustls", not(feature = "compio")))]
    pub(crate) async fn connect_tls_local_with_hint_observed_with_owner<S, F>(
        &self,
        _tcp_stream: S,
        _host: &str,
        _protocol_hint: ProtocolHint,
        observation: Option<LocalConnectionObservationOwner>,
        _on_tls_complete: F,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        F: FnOnce(&(), std::time::Duration),
    {
        let error = Error::Tls("TLS with !Send streams requires the compio feature".into());
        if let Some(observation) = observation.as_ref() {
            observation.establishment_failed(&error);
        }
        Err(error)
    }

    #[cfg(not(feature = "rustls"))]
    #[allow(dead_code)]
    pub(crate) async fn connect_tls_local_with_hint<S>(
        &self,
        _tcp_stream: S,
        _host: &str,
        _protocol_hint: ProtocolHint,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    {
        self.connect_tls_local_with_hint_observed_with_owner(
            _tcp_stream,
            _host,
            _protocol_hint,
            self.begin_connection_observation(),
            |_, _| {},
        )
        .await
    }

    #[cfg(not(feature = "rustls"))]
    pub(crate) async fn connect_tls_local_with_hint_observed_with_owner<S, F>(
        &self,
        _tcp_stream: S,
        _host: &str,
        _protocol_hint: ProtocolHint,
        observation: Option<LocalConnectionObservationOwner>,
        _on_tls_complete: F,
    ) -> Result<PooledConnection<RequestBodyLocal>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        F: FnOnce(&(), std::time::Duration),
    {
        let error = Error::Tls("HTTPS requires the `rustls` TLS backend feature".into());
        if let Some(observation) = observation.as_ref() {
            observation.establishment_failed(&error);
        }
        Err(error)
    }
}
