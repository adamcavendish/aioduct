use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::pool::ConnectionPool;
use crate::runtime::{Connector, Resolve, RuntimeLocal};
#[cfg(feature = "rustls")]
use crate::tls::TlsVersion;

use super::builder::HttpEngineBuilder;
use super::{HttpEngineCore, HttpEngineLocal};

impl<R: RuntimeLocal, C: Connector + Clone> HttpEngineBuilder<R, C> {
    #[allow(unreachable_code)]
    fn default_local_resolver() -> Option<Arc<dyn crate::runtime::Resolve>> {
        #[cfg(feature = "compio")]
        {
            return Some(Arc::new(crate::runtime::compio_rt::DefaultResolver));
        }
        None
    }

    /// Build the configured [`HttpEngineLocal`] for a completion-based runtime.
    pub fn build_local(self) -> HttpEngineLocal<R, C> {
        let pool = if self.no_connection_reuse {
            ConnectionPool::new(0, Duration::from_secs(0))
        } else {
            ConnectionPool::new(self.pool_max_idle_per_host, self.pool_idle_timeout)
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
                        TlsVersion::filter_versions(self.min_tls_version, self.max_tls_version)
                    } else {
                        vec![&rustls::version::TLS12, &rustls::version::TLS13]
                    };

                if needs_configured {
                    let mut root_store = rustls::RootCertStore::from_iter(
                        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
                    );
                    for cert in &self.extra_root_certs {
                        #[allow(clippy::expect_used)]
                        root_store
                            .add(cert.der.clone())
                            .expect("invalid extra root certificate");
                    }
                    let crls: Vec<_> = self.crls.into_iter().map(|c| c.der).collect();
                    let identity = self.client_identity.map(|id| (id.certs, id.key));
                    Some(Arc::new(
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

        HttpEngineLocal {
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
                resolver: {
                    if let Some(overrides) = self.static_resolves {
                        let fallback = self.resolver.or_else(|| Self::default_local_resolver());
                        let mut sr = crate::runtime::StaticResolver::new(fallback);
                        for (host, addrs) in overrides {
                            sr.add(host, addrs);
                        }
                        Some(Arc::new(sr) as Arc<dyn Resolve>)
                    } else {
                        self.resolver.or_else(|| Self::default_local_resolver())
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
                h3_endpoint: None,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                prefer_h3: false,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                h3_zero_rtt: false,
                #[cfg(all(feature = "http3", feature = "rustls"))]
                alt_svc_cache: crate::alt_svc::AltSvcCache::new(),
            },
            connector: self.connector,
            _phantom: PhantomData,
        }
    }
}
