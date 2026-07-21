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
    /// Enable async automatic RFC 9421 request signing for native send requests.
    ///
    /// The signer runs after default headers, cookies, cache validators,
    /// middleware, automatic `Content-Digest`, and digest-auth retry headers
    /// have finalized each request attempt. It receives an owned signature base,
    /// so request and header borrows do not cross the signer await boundary.
    ///
    /// The returned signing future must be [`Send`]. Use
    /// [`message_signature_async_local`](Self::message_signature_async_local) for
    /// local-runtime signing futures that are not `Send`.
    pub fn message_signature_async(
        mut self,
        config: crate::message_signatures::MessageSignatureConfig,
        signer: impl crate::message_signatures::MessageSignatureAsyncSigner,
    ) -> Self {
        self.message_signature = Some(
            crate::message_signatures::AutomaticMessageSignature::new_async_send(
                config,
                Arc::new(signer),
            ),
        );
        self
    }

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
    /// Request HTTP/3 0-RTT (early data) for repeat connections.
    ///
    /// HTTP/3 0-RTT is not currently supported. Setting `enable` to `true`
    /// causes [`build`](Self::build) to return
    /// [`Error::Unsupported`](crate::error::Error::Unsupported). Aioduct must
    /// retain and validate the peer's HTTP/3 settings before it can safely send
    /// early data, and the upstream `h3` API does not currently expose the
    /// required rejection handling.
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
            let endpoint =
                crate::h3_transport::build_quinn_endpoint(tls_config, self.local_address)
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
        #[cfg(feature = "compio")]
        {
            return Some(Arc::new(crate::runtime::compio_rt::DefaultResolver));
        }
        None
    }

    /// Build the configured [`HttpEngineSend`].
    pub fn build(self) -> Result<HttpEngineSend<R, C>, crate::error::Error> {
        let mut this = self;
        if let Some(error) = this.builder_error.take() {
            return Err(error.into_error());
        }
        #[cfg(all(feature = "http3", feature = "rustls"))]
        if this.h3_zero_rtt {
            return Err(crate::error::Error::Unsupported(
                "HTTP/3 0-RTT is not supported: peer settings and early-data rejection cannot yet be validated"
                    .to_owned(),
            ));
        }
        let self_ = this;

        let pool = if self_.no_connection_reuse {
            ConnectionPool::new()
                .with_max_idle_per_host(0)
                .with_idle_timeout(Duration::ZERO)
        } else {
            let mut pool = ConnectionPool::new()
                .with_max_idle_per_host(self_.pool_max_idle_per_host)
                .with_max_active_per_host(self_.pool_max_active_per_host)
                .with_idle_timeout(self_.pool_idle_timeout);
            if let Some(max_active) = self_.pool_max_active_streams_per_connection {
                pool = pool.with_max_active_streams_per_connection(max_active);
            }
            if let Some(max_lifetime) = self_.pool_max_lifetime {
                pool.with_max_lifetime(max_lifetime)
            } else {
                pool
            }
        };

        #[cfg(feature = "rustls")]
        let tls = {
            let has_version_constraints =
                self_.min_tls_version.is_some() || self_.max_tls_version.is_some();
            let has_extra_config =
                !self_.extra_root_certs.is_empty() || self_.client_identity.is_some();
            let has_crls = !self_.crls.is_empty();
            let needs_configured = has_crls || self_.danger_accept_invalid_hostnames;
            let needs_sni_update = self_.tls_sni == Some(false);

            let mut connector = if self_.tls.is_some()
                && !has_version_constraints
                && !has_extra_config
                && !needs_configured
            {
                self_.tls
            } else if needs_configured || has_extra_config || has_version_constraints {
                let versions: Vec<&'static rustls::SupportedProtocolVersion> =
                    if has_version_constraints {
                        TlsVersion::filter_versions(self_.min_tls_version, self_.max_tls_version)?
                    } else {
                        vec![&rustls::version::TLS12, &rustls::version::TLS13]
                    };

                if needs_configured {
                    let mut root_store = rustls::RootCertStore::from_iter(
                        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
                    );
                    for cert in &self_.extra_root_certs {
                        // SAFETY: extra root certs are caller-provided; if they
                        // are malformed the builder cannot continue.
                        #[allow(clippy::expect_used)]
                        root_store
                            .add(cert.der.clone())
                            .expect("invalid extra root certificate");
                    }
                    let crls: Vec<_> = self_.crls.into_iter().map(|c| c.der).collect();
                    let identity = self_.client_identity.map(|id| (id.certs, id.key));
                    Some(Arc::new(
                        // SAFETY: build_configured can only fail with invalid
                        // CRLs or client identity — caller-provided inputs that
                        // must be correct for the client to function.
                        #[allow(clippy::expect_used)]
                        crate::tls::RustlsConnector::build_configured(
                            root_store,
                            &versions,
                            crls,
                            self_.danger_accept_invalid_hostnames,
                            identity,
                        )
                        .expect(
                            "failed to build TLS configuration — check CRLs and client identity",
                        ),
                    ))
                } else if let Some(identity) = self_.client_identity {
                    Some(Arc::new(
                        // SAFETY: with_identity_versioned can only fail with an
                        // invalid client cert/key pair — caller-provided input.
                        #[allow(clippy::expect_used)]
                        crate::tls::RustlsConnector::with_identity_versioned(
                            &self_.extra_root_certs,
                            identity,
                            &versions,
                        )
                        .expect("failed to build TLS configuration — check client identity (cert/key pair)"),
                    ))
                } else if !self_.extra_root_certs.is_empty() {
                    Some(Arc::new(
                        crate::tls::RustlsConnector::with_extra_roots_versioned(
                            &self_.extra_root_certs,
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
                base_url: self_.base_url,
                address_family: self_.address_family,
                redirect_policy: self_.redirect_policy,
                timeout: self_.timeout,
                connect_timeout: self_.connect_timeout,
                read_timeout: self_.read_timeout,
                write_timeout: self_.write_timeout,
                tcp_keepalive: self_.tcp_keepalive,
                tcp_keepalive_interval: self_.tcp_keepalive_interval,
                tcp_keepalive_retries: self_.tcp_keepalive_retries,
                local_address: self_.local_address,
                #[cfg(target_os = "linux")]
                interface: self_.interface,
                #[cfg(unix)]
                unix_socket: self_.unix_socket,
                https_only: self_.https_only,
                referer: self_.referer,
                no_connection_reuse: self_.no_connection_reuse,
                tcp_fast_open: self_.tcp_fast_open,
                accept_encoding_header: self_.accept_encoding.header_value(),
                accept_encoding: self_.accept_encoding,
                default_headers: Arc::new(self_.default_headers),
                retry: self_.retry,
                cookie_jar: self_.cookie_jar,
                proxy: self_.proxy,
                proxy_chain: self_.proxy_chain,
                resolver: {
                    if let Some(overrides) = self_.static_resolves {
                        let fallback = self_.resolver.or_else(|| Self::default_resolver());
                        let mut sr = crate::runtime::StaticResolver::new(fallback);
                        for (host, addrs) in overrides {
                            sr.add(host, addrs);
                        }
                        Some(Arc::new(sr) as Arc<dyn Resolve>)
                    } else {
                        self_.resolver.or_else(|| Self::default_resolver())
                    }
                },
                http2: self_.http2,
                middleware: self_.middleware,
                rate_limiter: self_.rate_limiter,
                bandwidth_limiter: self_.bandwidth_limiter,
                digest_auth: self_.digest_auth,
                message_signature: self_.message_signature,
                automatic_content_digest: self_.automatic_content_digest,
                cache: self_.cache,
                hsts: self_.hsts,
                h2c_probe_cache: self_
                    .h2c_probe_ttl
                    .map(crate::h2c_probe::H2cProbeCache::with_ttl)
                    .unwrap_or_else(crate::h2c_probe::H2cProbeCache::new),
                connection_coalescing: self_.connection_coalescing,
                sensitive_headers: self_.sensitive_headers,
                observer: self_.observer,
                #[cfg(feature = "rustls")]
                tls,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                h3_endpoint: self_.h3_endpoint,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                prefer_h3: self_.prefer_h3,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                alt_svc_cache: crate::alt_svc::AltSvcCache::new(),
            },
            connector: self_.connector,
            #[cfg(feature = "tower")]
            tower_connector: self_.tower_connector,
            _phantom: PhantomData,
        })
    }
}
