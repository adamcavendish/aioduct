use std::future::Future;
use std::pin::Pin;

use crate::body::RequestBodySend;
use crate::error::Error;
use crate::observer::ConnectionObservationOwner;
use crate::pool::{PooledConnection, ProtocolHint};
use crate::runtime::{ConnectorSend, RuntimePoll};

use super::HttpEngineSend;

impl<R: RuntimePoll, C: ConnectorSend, O> HttpEngineSend<R, C, O>
where
    O: crate::observer::Observer + Send + Sync + 'static,
    O::ConnectionObservation: Send + Sync,
{
    pub(crate) fn begin_connection_observation(&self) -> Option<ConnectionObservationOwner>
    where
        O: crate::observer::Observer,
        O::ConnectionObservation: Send + Sync,
    {
        ConnectionObservationOwner::send(&*self.observer)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn connect_plaintext_with_hint<S>(
        &self,
        stream: S,
        force_h2c: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodySend>, Error>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.connect_plaintext_with_hint_observed(
            stream,
            force_h2c,
            self.begin_connection_observation(),
        )
    }

    pub(crate) fn connect_plaintext_with_hint_observed<S>(
        &self,
        stream: S,
        force_h2c: bool,
        observation: Option<ConnectionObservationOwner>,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<RequestBodySend>, Error>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        if force_h2c {
            Box::pin(self.connect_h2_prior_knowledge_with_observation(stream, observation))
        } else {
            Box::pin(self.connect_h1_with_observation(stream, observation))
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn connect_h1<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.connect_h1_with_observation(stream, self.begin_connection_observation())
            .await
    }

    pub(crate) async fn connect_h1_with_observation<S>(
        &self,
        stream: S,
        observation: Option<ConnectionObservationOwner>,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let transport = observation
            .as_ref()
            .and_then(crate::observer::transport::SendTransportObservation::new);
        if let Some(transport) = transport {
            let stream = crate::observer::transport::ObservedIo::new(stream, transport.clone());
            self.handshake_h1(stream, observation, Some(transport))
                .await
        } else {
            self.handshake_h1(stream, observation, None).await
        }
    }

    async fn handshake_h1<S>(
        &self,
        stream: S,
        observation: Option<ConnectionObservationOwner>,
        transport: Option<crate::observer::transport::SendTransportObservation>,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
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
        let mut pooled = PooledConnection::new_h1_with_observation(sender, observation);
        pooled.observe_established();
        if let Some(transport) = &transport {
            transport.established();
        }
        R::spawn_send(async move {
            let error = conn
                .with_upgrades()
                .await
                .err()
                .map(crate::error::Error::from);
            if let Some(transport) = transport {
                transport.driver_ended(error);
            }
        });
        Ok(pooled)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn connect_h2_prior_knowledge<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.connect_h2_prior_knowledge_with_observation(
            stream,
            self.begin_connection_observation(),
        )
        .await
    }

    pub(crate) async fn connect_h2_prior_knowledge_with_observation<S>(
        &self,
        stream: S,
        observation: Option<ConnectionObservationOwner>,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.handshake_h2(stream, observation, None).await
    }

    async fn handshake_h2<S>(
        &self,
        stream: S,
        observation: Option<ConnectionObservationOwner>,
        confirmation: Option<super::h2_peer_settings::H2PeerSettingsConfirmation>,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let mut builder = hyper::client::conn::http2::Builder::new(
            crate::runtime::executor::poll_executor::<R>(),
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

        let mut pooled = PooledConnection::new_h2_with_observation(sender, observation);
        pooled.observe_established();
        let metrics = std::sync::Arc::clone(&pooled.metrics);
        R::spawn_send(async move {
            let result = conn.await.map_err(crate::error::Error::from);
            metrics.observe_closed(result.err());
        });
        Ok(pooled)
    }

    pub(crate) async fn connect_h2_prior_knowledge_confirmed_with_observation<S>(
        &self,
        stream: S,
        observation: Option<ConnectionObservationOwner>,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
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
        self.handshake_h2(stream, observation, Some(confirmation))
            .await
    }

    #[cfg(all(feature = "rustls", feature = "tokio", test))]
    pub(crate) async fn connect_tls<S>(
        &self,
        tcp_stream: S,
        host: &str,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.connect_tls_with_hint(tcp_stream, host, ProtocolHint::Auto)
            .await
    }

    #[cfg(feature = "rustls")]
    #[allow(dead_code)]
    pub(crate) async fn connect_tls_with_hint<S>(
        &self,
        tcp_stream: S,
        host: &str,
        protocol_hint: ProtocolHint,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.connect_tls_with_hint_observed(tcp_stream, host, protocol_hint, |_, _| {})
            .await
    }

    #[cfg(feature = "rustls")]
    pub(crate) async fn connect_tls_with_hint_observed<S, F>(
        &self,
        tcp_stream: S,
        host: &str,
        protocol_hint: ProtocolHint,
        on_tls_complete: F,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
        F: FnOnce(&crate::tls::TlsStream<S>, std::time::Duration),
    {
        self.connect_tls_with_hint_observed_with_owner(
            tcp_stream,
            host,
            protocol_hint,
            self.begin_connection_observation(),
            on_tls_complete,
        )
        .await
    }

    #[cfg(feature = "rustls")]
    pub(crate) async fn connect_tls_with_hint_observed_with_owner<S, F>(
        &self,
        tcp_stream: S,
        host: &str,
        protocol_hint: ProtocolHint,
        observation: Option<ConnectionObservationOwner>,
        on_tls_complete: F,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
        F: FnOnce(&crate::tls::TlsStream<S>, std::time::Duration),
    {
        use crate::tls::TlsConnect;
        use std::time::Instant;

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

        #[cfg(feature = "tracing")]
        tracing::trace!(host = host, "tls.handshake.start");

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

        let tls_stream = <crate::tls::RustlsConnector as TlsConnect<S>>::connect(
            &tls_connector,
            host,
            tcp_stream,
        )
        .await
        .map_err(|e| {
            #[cfg(feature = "tracing")]
            tracing::trace!(host = host, error = %e, "tls.handshake.error");
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

        #[cfg(feature = "tracing")]
        tracing::trace!(
            host = host,
            alpn = ?alpn,
            "tls.handshake.done",
        );
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
                    crate::runtime::executor::poll_executor::<R>(),
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
                let mut pooled = PooledConnection::new_h2_with_observation(sender, observation);
                pooled.observe_established();
                let metrics = std::sync::Arc::clone(&pooled.metrics);
                R::spawn_send(async move {
                    let result = conn.await.map_err(crate::error::Error::from);
                    metrics.observe_closed(result.err());
                });
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
            _ => {
                let mut pooled = self
                    .connect_h1_with_observation(tls_stream, observation)
                    .await?;
                pooled.tls_info = Some(tls_info);
                pooled.tls_handshake_duration = Some(tls_duration);
                Ok(pooled)
            }
        }
    }

    #[cfg(not(feature = "rustls"))]
    #[allow(dead_code)]
    pub(crate) async fn connect_tls_with_hint<S>(
        &self,
        _tcp_stream: S,
        _host: &str,
        _protocol_hint: ProtocolHint,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        self.connect_tls_with_hint_observed_with_owner(
            _tcp_stream,
            _host,
            _protocol_hint,
            self.begin_connection_observation(),
            |_, _| {},
        )
        .await
    }

    #[cfg(not(feature = "rustls"))]
    pub(crate) async fn connect_tls_with_hint_observed_with_owner<S, F>(
        &self,
        _tcp_stream: S,
        _host: &str,
        _protocol_hint: ProtocolHint,
        observation: Option<ConnectionObservationOwner>,
        _on_tls_complete: F,
    ) -> Result<PooledConnection<RequestBodySend>, Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
        F: FnOnce(&(), std::time::Duration),
    {
        let error = Error::Tls("HTTPS requires the `rustls` TLS backend feature".into());
        if let Some(observation) = observation.as_ref() {
            observation.establishment_failed(&error);
        }
        Err(error)
    }
}
