use std::collections::HashSet;
use std::marker::PhantomData;
use std::net::IpAddr;
use std::num::NonZeroUsize;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use http::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};

use crate::cache::HttpCache;
use crate::cookie::CookieJar;
use crate::http2::Http2Config;
use crate::middleware::MiddlewareStack;
use crate::proxy::{ProxyChain, ProxySettings};
use crate::redirect::RedirectPolicy;
use crate::retry::RetryConfig;
use crate::runtime::Resolve;
#[cfg(feature = "rustls")]
use crate::tls::TlsVersion;

use super::DEFAULT_USER_AGENT;

/// Builder for configuring an [`HttpEngineSend`](super::HttpEngineSend).
pub struct HttpEngineBuilder<R, C> {
    pub(super) connector: C,
    pub(super) base_url: Option<Arc<url::Url>>,
    pub(super) pool_idle_timeout: Duration,
    pub(super) pool_max_lifetime: Option<Duration>,
    pub(super) pool_max_idle_per_host: usize,
    pub(super) pool_max_active_per_host: Option<NonZeroUsize>,
    pub(super) pool_max_active_streams_per_connection: Option<NonZeroUsize>,
    pub(super) no_connection_reuse: bool,
    pub(super) tcp_fast_open: bool,
    pub(super) redirect_policy: RedirectPolicy,
    pub(super) timeout: Option<Duration>,
    pub(super) connect_timeout: Option<Duration>,
    pub(super) read_timeout: Option<Duration>,
    pub(super) write_timeout: Option<Duration>,
    pub(super) tcp_keepalive: Option<Duration>,
    pub(super) tcp_keepalive_interval: Option<Duration>,
    pub(super) tcp_keepalive_retries: Option<u32>,
    pub(super) local_address: Option<IpAddr>,
    #[cfg(target_os = "linux")]
    pub(super) interface: Option<String>,
    #[cfg(unix)]
    pub(super) unix_socket: Option<PathBuf>,
    pub(super) https_only: bool,
    pub(super) referer: bool,
    pub(super) accept_encoding: crate::decompress::AcceptEncoding,
    pub(super) default_headers: HeaderMap,
    pub(super) retry: Option<RetryConfig>,
    pub(super) cookie_jar: Option<CookieJar>,
    pub(super) proxy: Option<ProxySettings>,
    pub(super) proxy_chain: Option<ProxyChain>,
    pub(super) resolver: Option<Arc<dyn Resolve>>,
    pub(super) static_resolves:
        Option<std::collections::HashMap<String, Vec<std::net::SocketAddr>>>,
    pub(super) http2: Option<Http2Config>,
    pub(super) middleware: MiddlewareStack,
    pub(super) rate_limiter: Option<crate::throttle::RateLimiter>,
    pub(super) bandwidth_limiter: Option<crate::bandwidth::BandwidthLimiter>,
    pub(super) digest_auth: Option<crate::digest_auth::DigestAuth>,
    pub(super) cache: Option<HttpCache>,
    pub(super) hsts: Option<crate::hsts::HstsStore>,
    pub(super) h2c_probe_ttl: Option<Duration>,
    pub(super) connection_coalescing: bool,
    pub(super) sensitive_headers: HashSet<HeaderName>,
    pub(super) observer: Option<Arc<dyn crate::observer::RequestObserver>>,
    #[cfg(feature = "tower")]
    pub(super) tower_connector: Option<crate::connector::TowerConnectorSendSlot>,
    #[cfg(feature = "tower")]
    pub(super) tower_connector_local: Option<crate::connector::TowerConnectorLocalSlot>,
    #[cfg(feature = "rustls")]
    pub(super) tls: Option<Arc<crate::tls::RustlsConnector>>,
    #[cfg(feature = "rustls")]
    pub(super) min_tls_version: Option<TlsVersion>,
    #[cfg(feature = "rustls")]
    pub(super) max_tls_version: Option<TlsVersion>,
    #[cfg(feature = "rustls")]
    pub(super) tls_sni: Option<bool>,
    #[cfg(feature = "rustls")]
    pub(super) extra_root_certs: Vec<crate::tls::Certificate>,
    #[cfg(feature = "rustls")]
    pub(super) client_identity: Option<crate::tls::Identity>,
    #[cfg(feature = "rustls")]
    pub(super) crls: Vec<crate::tls::CertificateRevocationList>,
    #[cfg(feature = "rustls")]
    pub(super) danger_accept_invalid_hostnames: bool,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(super) h3_endpoint: Option<quinn::Endpoint>,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(super) prefer_h3: bool,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(super) h3_zero_rtt: bool,
    pub(super) _phantom: PhantomData<(R, C)>,
}

impl<R, C> HttpEngineBuilder<R, C> {
    /// Create a new builder with the given connector and default settings.
    pub fn new(connector: C) -> Self {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));

        Self {
            connector,
            base_url: None,
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_lifetime: None,
            pool_max_idle_per_host: 10,
            pool_max_active_per_host: None,
            pool_max_active_streams_per_connection: None,
            no_connection_reuse: false,
            tcp_fast_open: false,
            redirect_policy: RedirectPolicy::default(),
            timeout: None,
            connect_timeout: None,
            read_timeout: None,
            write_timeout: None,
            tcp_keepalive: None,
            tcp_keepalive_interval: None,
            tcp_keepalive_retries: None,
            local_address: None,
            #[cfg(target_os = "linux")]
            interface: None,
            #[cfg(unix)]
            unix_socket: None,
            https_only: false,
            referer: false,
            accept_encoding: crate::decompress::AcceptEncoding::default(),
            default_headers,
            retry: None,
            cookie_jar: None,
            proxy: None,
            proxy_chain: None,
            resolver: None,
            static_resolves: None,
            http2: None,
            middleware: MiddlewareStack::new(),
            rate_limiter: None,
            bandwidth_limiter: None,
            digest_auth: None,
            cache: None,
            hsts: None,
            h2c_probe_ttl: None,
            connection_coalescing: true,
            sensitive_headers: HashSet::new(),
            observer: None,
            #[cfg(feature = "tower")]
            tower_connector: None,
            #[cfg(feature = "tower")]
            tower_connector_local: None,
            #[cfg(feature = "rustls")]
            tls: None,
            #[cfg(feature = "rustls")]
            min_tls_version: None,
            #[cfg(feature = "rustls")]
            max_tls_version: None,
            #[cfg(feature = "rustls")]
            tls_sni: None,
            #[cfg(feature = "rustls")]
            extra_root_certs: Vec::new(),
            #[cfg(feature = "rustls")]
            client_identity: None,
            #[cfg(feature = "rustls")]
            crls: Vec::new(),
            #[cfg(feature = "rustls")]
            danger_accept_invalid_hostnames: false,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            h3_endpoint: None,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            prefer_h3: false,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            h3_zero_rtt: false,
            _phantom: PhantomData,
        }
    }
}

impl<R, C> std::fmt::Debug for HttpEngineBuilder<R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEngineBuilder").finish()
    }
}
