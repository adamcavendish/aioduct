use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::pool::ConnectionPool;
use crate::runtime::{Connector, Resolve, RuntimeLocal};

use super::HttpEngine;
use super::builder::HttpEngineBuilder;

impl<R: RuntimeLocal, C: Connector + Clone> HttpEngineBuilder<R, C> {
    /// Build the configured [`HttpEngine`] for a completion-based runtime.
    pub fn build_local(self) -> HttpEngine<R, C> {
        let pool = if self.no_connection_reuse {
            ConnectionPool::new(0, Duration::from_secs(0))
        } else {
            ConnectionPool::new(self.pool_max_idle_per_host, self.pool_idle_timeout)
        };

        #[cfg(feature = "rustls")]
        let tls = self.tls;

        HttpEngine {
            pool,
            connector: self.connector,
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
            accept_encoding: self.accept_encoding,
            default_headers: self.default_headers,
            retry: self.retry,
            cookie_jar: self.cookie_jar,
            proxy: self.proxy,
            resolver: {
                if let Some(overrides) = self.static_resolves {
                    let fallback = self.resolver;
                    let mut sr = crate::runtime::StaticResolver::new(fallback);
                    for (host, addrs) in overrides {
                        sr.add(host, addrs);
                    }
                    Some(Arc::new(sr) as Arc<dyn Resolve>)
                } else {
                    self.resolver
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
            connection_coalescing: false,
            observer: self.observer,
            #[cfg(feature = "tower")]
            tower_connector: None,
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
            _phantom: PhantomData,
        }
    }
}
