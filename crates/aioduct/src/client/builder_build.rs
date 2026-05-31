use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::pool::ConnectionPool;
use crate::runtime::{ConnectorSend, Resolve, RuntimePoll};
#[cfg(feature = "rustls")]
use crate::tls::TlsVersion;

use super::builder::HttpEngineBuilder;
use super::{HttpEngineCore, HttpEngineSend};

// ── Send path only (tower, h3, build) ────────────────────────────────────────

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineBuilder<R, C> {
    #[cfg(feature = "tower")]
    /// Wrap the TCP connector with a tower `Layer`.
    ///
    /// The layer wraps the default runtime connector, which connects to a
    /// resolved `SocketAddr`. Use this to add cross-cutting transport concerns
    /// like metrics, tracing, or connection-level rate limiting.
    pub fn connector_layer<L>(mut self, layer: L) -> Self
    where
        L: tower_layer::Layer<crate::connector::ConnectorServiceSend<C>>,
        L::Service: tower_service::Service<
                crate::connector::ConnectInfo,
                Response = C::Stream,
                Error = std::io::Error,
            > + Send
            + Sync
            + Clone
            + 'static,
        <L::Service as tower_service::Service<crate::connector::ConnectInfo>>::Future:
            Send + 'static,
    {
        self.tower_connector = Some(crate::connector::TowerConnectorSendSlot::new(
            crate::connector::apply_layer_send(self.connector.clone(), layer),
        ));
        self
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Enable or disable HTTP/3 for all HTTPS requests.
    pub fn http3(mut self, enable: bool) -> Result<Self, crate::error::Error> {
        if enable {
            self = self.ensure_h3_endpoint()?;
            self.prefer_h3 = true;
        } else {
            self.h3_endpoint = None;
            self.prefer_h3 = false;
        }
        Ok(self)
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Enable automatic HTTP/3 upgrade via Alt-Svc headers.
    pub fn alt_svc_h3(mut self, enable: bool) -> Result<Self, crate::error::Error> {
        if enable {
            self = self.ensure_h3_endpoint()?;
        } else if !self.prefer_h3 {
            self.h3_endpoint = None;
        }
        Ok(self)
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Enable HTTP/3 0-RTT (early data) for repeat connections.
    ///
    /// When enabled, idempotent requests (GET, HEAD, OPTIONS) may be sent
    /// before the TLS handshake completes on reconnection to a previously-visited
    /// server. Non-idempotent requests always wait for the full handshake.
    ///
    /// Opt-in because 0-RTT data is replayable by a network attacker.
    pub fn h3_zero_rtt(mut self, enable: bool) -> Self {
        self.h3_zero_rtt = enable;
        self
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    fn ensure_h3_endpoint(mut self) -> Result<Self, crate::error::Error> {
        if self.h3_endpoint.is_none() {
            let tls_config = self
                .tls
                .as_ref()
                .ok_or_else(|| {
                    crate::error::Error::Other(
                        "HTTP/3 requires a TLS connector — call .tls() before .http3(true)".into(),
                    )
                })?
                .config()
                .clone();
            let endpoint = crate::h3_transport::build_quinn_endpoint(
                tls_config,
                self.local_address,
                self.h3_zero_rtt,
            )
            .map_err(|e| crate::error::Error::Other(Box::new(e)))?;
            self.h3_endpoint = Some(endpoint);
        }
        Ok(self)
    }

    #[allow(unreachable_code)]
    fn default_resolver() -> Option<Arc<dyn crate::runtime::Resolve>> {
        #[cfg(feature = "tokio")]
        {
            return Some(Arc::new(crate::runtime::tokio_rt::DefaultResolver));
        }
        #[cfg(feature = "smol")]
        {
            return Some(Arc::new(crate::runtime::smol_rt::DefaultResolver));
        }
        None
    }

    /// Build the configured [`HttpEngineSend`].
    pub fn build(self) -> Result<HttpEngineSend<R, C>, crate::error::Error> {
        let pool = if self.no_connection_reuse {
            ConnectionPool::new(0, Duration::from_secs(0))
        } else {
            let mut pool = ConnectionPool::new(self.pool_max_idle_per_host, self.pool_idle_timeout);
            if let Some(max_active) = self.pool_max_active_streams_per_connection {
                pool = pool.with_max_active_streams_per_connection(max_active);
            }
            if let Some(max_lifetime) = self.pool_max_lifetime {
                pool.with_max_lifetime(max_lifetime)
            } else {
                pool
            }
        };

        #[cfg(feature = "rustls")]
        let tls = {
            let has_version_constraints =
                self.min_tls_version.is_some() || self.max_tls_version.is_some();
            let has_extra_config =
                !self.extra_root_certs.is_empty() || self.client_identity.is_some();
            let has_crls = !self.crls.is_empty();
            let needs_configured = has_crls || self.danger_accept_invalid_hostnames;
            let needs_sni_update = self.tls_sni == Some(false);

            let mut connector = if self.tls.is_some()
                && !has_version_constraints
                && !has_extra_config
                && !needs_configured
            {
                self.tls
            } else if needs_configured || has_extra_config || has_version_constraints {
                let versions: Vec<&'static rustls::SupportedProtocolVersion> =
                    if has_version_constraints {
                        TlsVersion::filter_versions(self.min_tls_version, self.max_tls_version)?
                    } else {
                        vec![&rustls::version::TLS12, &rustls::version::TLS13]
                    };

                if needs_configured {
                    let mut root_store = rustls::RootCertStore::from_iter(
                        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
                    );
                    for cert in &self.extra_root_certs {
                        // SAFETY: extra root certs are caller-provided; if they
                        // are malformed the builder cannot continue.
                        #[allow(clippy::expect_used)]
                        root_store
                            .add(cert.der.clone())
                            .expect("invalid extra root certificate");
                    }
                    let crls: Vec<_> = self.crls.into_iter().map(|c| c.der).collect();
                    let identity = self.client_identity.map(|id| (id.certs, id.key));
                    Some(Arc::new(
                        // SAFETY: build_configured can only fail with invalid
                        // CRLs or client identity — caller-provided inputs that
                        // must be correct for the client to function.
                        #[allow(clippy::expect_used)]
                        crate::tls::RustlsConnector::build_configured(
                            root_store,
                            &versions,
                            crls,
                            self.danger_accept_invalid_hostnames,
                            identity,
                        )
                        .expect(
                            "failed to build TLS configuration — check CRLs and client identity",
                        ),
                    ))
                } else if let Some(identity) = self.client_identity {
                    Some(Arc::new(
                        // SAFETY: with_identity_versioned can only fail with an
                        // invalid client cert/key pair — caller-provided input.
                        #[allow(clippy::expect_used)]
                        crate::tls::RustlsConnector::with_identity_versioned(
                            &self.extra_root_certs,
                            identity,
                            &versions,
                        )
                        .expect("failed to build TLS configuration — check client identity (cert/key pair)"),
                    ))
                } else if !self.extra_root_certs.is_empty() {
                    Some(Arc::new(
                        crate::tls::RustlsConnector::with_extra_roots_versioned(
                            &self.extra_root_certs,
                            &versions,
                        ),
                    ))
                } else {
                    Some(Arc::new(
                        crate::tls::RustlsConnector::with_webpki_roots_versioned(&versions),
                    ))
                }
            } else {
                Some(Arc::new(crate::tls::RustlsConnector::with_webpki_roots()))
            };

            if needs_sni_update {
                let c = connector.get_or_insert_with(|| {
                    Arc::new(crate::tls::RustlsConnector::with_webpki_roots())
                });
                Arc::make_mut(c).config_mut().enable_sni = false;
            }

            connector
        };

        Ok(HttpEngineSend {
            core: HttpEngineCore {
                pool,
                redirect_policy: self.redirect_policy,
                timeout: self.timeout,
                connect_timeout: self.connect_timeout,
                read_timeout: self.read_timeout,
                tcp_keepalive: self.tcp_keepalive,
                tcp_keepalive_interval: self.tcp_keepalive_interval,
                tcp_keepalive_retries: self.tcp_keepalive_retries,
                local_address: self.local_address,
                #[cfg(target_os = "linux")]
                interface: self.interface,
                #[cfg(unix)]
                unix_socket: self.unix_socket,
                https_only: self.https_only,
                referer: self.referer,
                no_connection_reuse: self.no_connection_reuse,
                tcp_fast_open: self.tcp_fast_open,
                http2_prior_knowledge: self.http2_prior_knowledge,
                accept_encoding_header: self.accept_encoding.header_value(),
                accept_encoding: self.accept_encoding,
                default_headers: Arc::new(self.default_headers),
                retry: self.retry,
                cookie_jar: self.cookie_jar,
                proxy: self.proxy,
                proxy_chain: self.proxy_chain,
                resolver: {
                    if let Some(overrides) = self.static_resolves {
                        let fallback = self.resolver.or_else(|| Self::default_resolver());
                        let mut sr = crate::runtime::StaticResolver::new(fallback);
                        for (host, addrs) in overrides {
                            sr.add(host, addrs);
                        }
                        Some(Arc::new(sr) as Arc<dyn Resolve>)
                    } else {
                        self.resolver.or_else(|| Self::default_resolver())
                    }
                },
                http2: self.http2,
                middleware: self.middleware,
                rate_limiter: self.rate_limiter,
                bandwidth_limiter: self.bandwidth_limiter,
                digest_auth: self.digest_auth,
                cache: self.cache,
                hsts: self.hsts,
                h2c_probe_cache: self
                    .h2c_probe_ttl
                    .map(crate::h2c_probe::H2cProbeCache::with_ttl)
                    .unwrap_or_else(crate::h2c_probe::H2cProbeCache::new),
                connection_coalescing: self.connection_coalescing,
                sensitive_headers: self.sensitive_headers,
                observer: self.observer,
                #[cfg(feature = "rustls")]
                tls,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                h3_endpoint: self.h3_endpoint,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                prefer_h3: self.prefer_h3,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                h3_zero_rtt: self.h3_zero_rtt,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                alt_svc_cache: crate::alt_svc::AltSvcCache::new(),
            },
            connector: self.connector,
            #[cfg(feature = "tower")]
            tower_connector: self.tower_connector,
            _phantom: PhantomData,
        })
    }
}
