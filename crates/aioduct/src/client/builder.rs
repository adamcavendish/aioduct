use std::marker::PhantomData;
use std::net::IpAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use http::header::{HeaderMap, HeaderValue, USER_AGENT};

use crate::cache::HttpCache;
use crate::cookie::CookieJar;
use crate::http2::Http2Config;
use crate::middleware::{Middleware, MiddlewareStack};
use crate::pool::ConnectionPool;
use crate::proxy::{ProxyConfig, ProxySettings};
use crate::redirect::RedirectPolicy;
use crate::retry::RetryConfig;
use crate::runtime::{ConnectorSend, Resolve, RuntimePoll};
#[cfg(feature = "rustls")]
use crate::tls::TlsVersion;

use super::{DEFAULT_USER_AGENT, HttpEngine};

/// Builder for configuring an [`HttpEngine`].
pub struct HttpEngineBuilder<R: RuntimePoll, C: ConnectorSend> {
    pub(super) connector: C,
    pub(super) pool_idle_timeout: Duration,
    pub(super) pool_max_idle_per_host: usize,
    pub(super) no_connection_reuse: bool,
    pub(super) tcp_fast_open: bool,
    pub(super) http2_prior_knowledge: bool,
    pub(super) redirect_policy: RedirectPolicy,
    pub(super) timeout: Option<Duration>,
    pub(super) connect_timeout: Option<Duration>,
    pub(super) read_timeout: Option<Duration>,
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
    pub(super) observer: Option<Arc<dyn crate::observer::RequestObserver>>,
    #[cfg(feature = "tower")]
    pub(super) tower_connector: Option<crate::connector::LayeredConnector<C>>,
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

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineBuilder<R, C> {
    /// Create a new builder with the given connector and default settings.
    pub fn new(connector: C) -> Self {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));

        Self {
            connector,
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 10,
            no_connection_reuse: false,
            tcp_fast_open: false,
            http2_prior_knowledge: false,
            redirect_policy: RedirectPolicy::default(),
            timeout: None,
            connect_timeout: None,
            read_timeout: None,
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
            observer: None,
            #[cfg(feature = "tower")]
            tower_connector: None,
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

impl<R: RuntimePoll, C: ConnectorSend> std::fmt::Debug for HttpEngineBuilder<R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEngineBuilder").finish()
    }
}

impl<R: RuntimePoll, C: ConnectorSend> HttpEngineBuilder<R, C> {
    /// Set the idle connection timeout (default: 90s).
    pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.pool_idle_timeout = timeout;
        self
    }

    /// Set the max idle connections per host (default: 10).
    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.pool_max_idle_per_host = max;
        self
    }

    /// Set the maximum number of redirects to follow (default: 10).
    pub fn max_redirects(mut self, max: usize) -> Self {
        self.redirect_policy = RedirectPolicy::limited(max);
        self
    }

    /// Set a custom redirect policy.
    pub fn redirect_policy(mut self, policy: RedirectPolicy) -> Self {
        self.redirect_policy = policy;
        self
    }

    /// Set a default request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set a timeout for establishing connections (TCP + TLS handshake).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Set a timeout for gaps between body data chunks.
    ///
    /// This applies **only to response body reads**, not to waiting for
    /// response headers. If no body data arrives within this duration the
    /// request fails with [`Error::Timeout`](crate::Error::Timeout).
    ///
    /// Use [`timeout()`](Self::timeout) for an overall request deadline that
    /// covers headers and body, or [`connect_timeout()`](Self::connect_timeout)
    /// for the TCP + TLS handshake phase.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = Some(timeout);
        self
    }

    /// Enable TCP keepalive with the given idle time before first probe.
    pub fn tcp_keepalive(mut self, interval: Duration) -> Self {
        self.tcp_keepalive = Some(interval);
        self
    }

    /// Set the interval between TCP keepalive probes (platform-specific).
    pub fn tcp_keepalive_interval(mut self, interval: Duration) -> Self {
        self.tcp_keepalive_interval = Some(interval);
        self
    }

    /// Set the number of TCP keepalive probes before dropping (platform-specific).
    pub fn tcp_keepalive_retries(mut self, retries: u32) -> Self {
        self.tcp_keepalive_retries = Some(retries);
        self
    }

    /// Bind outgoing connections to a specific local IP address.
    pub fn local_address(mut self, addr: IpAddr) -> Self {
        self.local_address = Some(addr);
        self
    }

    #[cfg(target_os = "linux")]
    /// Bind outgoing connections to a specific network interface (Linux only).
    pub fn interface(mut self, name: impl Into<String>) -> Self {
        self.interface = Some(name.into());
        self
    }

    #[cfg(unix)]
    /// Route all requests through a Unix domain socket (e.g. Docker socket).
    ///
    /// The URI host is still sent in the `Host` header but the TCP connection
    /// is replaced by a connection to the given socket path.
    pub fn unix_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.unix_socket = Some(path.into());
        self
    }

    /// Only allow HTTPS URLs; reject plain HTTP requests with an error.
    pub fn https_only(mut self, enable: bool) -> Self {
        self.https_only = enable;
        self
    }

    /// Set the User-Agent header for all requests.
    ///
    /// If the value contains invalid header characters, it is silently ignored.
    pub fn user_agent(mut self, value: impl AsRef<str>) -> Self {
        if let Ok(val) = HeaderValue::from_str(value.as_ref()) {
            self.default_headers.insert(USER_AGENT, val);
        }
        self
    }

    /// Automatically set the `Referer` header on redirects (default: false).
    pub fn referer(mut self, enable: bool) -> Self {
        self.referer = enable;
        self
    }

    /// Disable connection pooling — each request opens a new connection.
    pub fn no_connection_reuse(mut self) -> Self {
        self.no_connection_reuse = true;
        self
    }

    /// Enable TCP Fast Open (RFC 7413) for reduced connection latency.
    ///
    /// On Linux, this sets `TCP_FASTOPEN_CONNECT` which allows the kernel to
    /// send data in the SYN packet for subsequent connections to known hosts.
    pub fn tcp_fast_open(mut self, enable: bool) -> Self {
        self.tcp_fast_open = enable;
        self
    }

    /// Use HTTP/2 prior knowledge (h2c) — send HTTP/2 over plaintext without upgrade.
    pub fn http2_prior_knowledge(mut self) -> Self {
        self.http2_prior_knowledge = true;
        self
    }

    /// Disable automatic response body decompression.
    pub fn no_decompression(mut self) -> Self {
        self.accept_encoding = crate::decompress::AcceptEncoding::none();
        self
    }

    /// Add headers sent with every request.
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers.extend(headers);
        self
    }

    /// Clear all default headers including User-Agent.
    pub fn no_default_headers(mut self) -> Self {
        self.default_headers.clear();
        self
    }

    /// Set a default retry configuration for all requests.
    pub fn retry(mut self, config: RetryConfig) -> Self {
        self.retry = Some(config);
        self
    }

    /// Enable cookie storage with the given jar.
    pub fn cookie_jar(mut self, jar: CookieJar) -> Self {
        self.cookie_jar = Some(jar);
        self
    }

    /// Route requests through an HTTP proxy (used for both HTTP and HTTPS targets).
    pub fn proxy(mut self, config: ProxyConfig) -> Self {
        self.proxy = Some(ProxySettings::all(config));
        self
    }

    /// Use proxy settings from environment variables (HTTP_PROXY, HTTPS_PROXY, NO_PROXY).
    pub fn system_proxy(mut self) -> Self {
        self.proxy = Some(ProxySettings::from_env());
        self
    }

    /// Set detailed proxy settings with separate HTTP/HTTPS proxies and bypass rules.
    pub fn proxy_settings(mut self, settings: ProxySettings) -> Self {
        self.proxy = Some(settings);
        self
    }

    /// Set a custom DNS resolver, overriding the runtime's default.
    pub fn resolver(mut self, resolver: impl Resolve) -> Self {
        self.resolver = Some(Arc::new(resolver));
        self
    }

    /// Override DNS resolution for a specific hostname.
    ///
    /// All requests to `domain` will connect to the given `addr` instead of
    /// performing DNS resolution. Multiple calls with different domains accumulate.
    ///
    /// # Example
    ///
    /// ```
    /// # use aioduct::{HttpEngine, runtime::TokioRuntime};
    /// # use aioduct::runtime::tokio_rt::TcpConnector;
    /// let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
    ///     .resolve("example.com", "127.0.0.1:8080".parse().unwrap())
    ///     .build();
    /// ```
    pub fn resolve(self, domain: &str, addr: std::net::SocketAddr) -> Self {
        self.resolve_to_addrs(domain, &[addr])
    }

    /// Override DNS resolution for a specific hostname with multiple addresses.
    ///
    /// The client will attempt connections to the provided addresses using Happy
    /// Eyeballs (RFC 8305) ordering. Multiple calls with different domains accumulate.
    pub fn resolve_to_addrs(mut self, domain: &str, addrs: &[std::net::SocketAddr]) -> Self {
        self.static_resolves
            .get_or_insert_with(Default::default)
            .insert(domain.to_owned(), addrs.to_vec());
        self
    }

    /// Use DNS-over-HTTPS for name resolution.
    ///
    /// Requires the `doh` feature. The `server_ip` should be the resolver's IP
    /// address (e.g., `"1.1.1.1"` for Cloudflare, `"8.8.8.8"` for Google).
    /// The `server_name` is the TLS hostname (e.g., `"cloudflare-dns.com"`).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aioduct::{HttpEngine, runtime::TokioRuntime};
    /// let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
    ///     .dns_over_https("1.1.1.1".parse().unwrap(), "cloudflare-dns.com")
    ///     .build();
    /// ```
    #[cfg(feature = "doh")]
    pub fn dns_over_https(self, server_ip: std::net::IpAddr, server_name: &str) -> Self {
        use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
        let ns = NameServerConfig::https(server_ip, std::sync::Arc::from(server_name), None);
        let config = ResolverConfig::from_parts(None, vec![], vec![ns]);
        let resolver = crate::HickoryResolver::from_config(config, ResolverOpts::default());
        self.resolver(resolver)
    }

    /// Use DNS-over-TLS for name resolution.
    ///
    /// Requires the `dot` feature. The `server_ip` should be the resolver's IP
    /// address (e.g., `"1.1.1.1"` for Cloudflare, `"8.8.8.8"` for Google).
    /// The `server_name` is the TLS hostname for certificate verification.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aioduct::{HttpEngine, runtime::TokioRuntime};
    /// let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
    ///     .dns_over_tls("1.1.1.1".parse().unwrap(), "cloudflare-dns.com")
    ///     .build();
    /// ```
    #[cfg(feature = "dot")]
    pub fn dns_over_tls(self, server_ip: std::net::IpAddr, server_name: &str) -> Self {
        use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
        let ns = NameServerConfig::tls(server_ip, std::sync::Arc::from(server_name));
        let config = ResolverConfig::from_parts(None, vec![], vec![ns]);
        let resolver = crate::HickoryResolver::from_config(config, ResolverOpts::default());
        self.resolver(resolver)
    }

    /// Configure HTTP/2 connection parameters (window sizes, keepalive, frame size).
    pub fn http2(mut self, config: Http2Config) -> Self {
        self.http2 = Some(config);
        self
    }

    /// Set the HTTP/2 initial stream-level flow-control window size (bytes).
    pub fn http2_initial_stream_window_size(mut self, size: u32) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .initial_stream_window_size = Some(size);
        self
    }

    /// Set the HTTP/2 initial connection-level flow-control window size (bytes).
    pub fn http2_initial_connection_window_size(mut self, size: u32) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .initial_connection_window_size = Some(size);
        self
    }

    /// Set the HTTP/2 max frame size (bytes).
    pub fn http2_max_frame_size(mut self, size: u32) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .max_frame_size = Some(size);
        self
    }

    /// Enable HTTP/2 adaptive flow-control window sizing.
    pub fn http2_adaptive_window(mut self, enabled: bool) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .adaptive_window = Some(enabled);
        self
    }

    /// Set the HTTP/2 PING keep-alive interval.
    pub fn http2_keep_alive_interval(mut self, interval: Duration) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .keep_alive_interval = Some(interval);
        self
    }

    /// Set the HTTP/2 PING acknowledgement timeout.
    pub fn http2_keep_alive_timeout(mut self, timeout: Duration) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .keep_alive_timeout = Some(timeout);
        self
    }

    /// Send HTTP/2 keep-alive PINGs even when idle.
    pub fn http2_keep_alive_while_idle(mut self, enabled: bool) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .keep_alive_while_idle = Some(enabled);
        self
    }

    /// Set the HTTP/2 max header list size (bytes).
    pub fn http2_max_header_list_size(mut self, size: u32) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .max_header_list_size = Some(size);
        self
    }

    /// Set the HTTP/2 max send buffer size per stream (bytes).
    pub fn http2_max_send_buf_size(mut self, size: usize) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .max_send_buf_size = Some(size);
        self
    }

    /// Set the max number of HTTP/2 locally-reset streams to keep in the reset state.
    pub fn http2_max_concurrent_reset_streams(mut self, max: usize) -> Self {
        self.http2
            .get_or_insert_with(Http2Config::new)
            .max_concurrent_reset_streams = Some(max);
        self
    }

    /// Set the TTL for adaptive h2c probe results (default: 5 minutes).
    pub fn h2c_probe_ttl(mut self, ttl: Duration) -> Self {
        self.h2c_probe_ttl = Some(ttl);
        self
    }

    /// Enable or disable HTTP/2 and HTTP/3 connection coalescing (default: enabled).
    ///
    /// When enabled, reuses connections whose TLS certificate SANs cover the
    /// target domain, provided DNS resolves to the same IP address.
    /// Matches browser behavior (RFC 7540 §9.1.1).
    pub fn connection_coalescing(mut self, enabled: bool) -> Self {
        self.connection_coalescing = enabled;
        self
    }

    /// Add a middleware layer that can inspect or modify requests and responses.
    pub fn middleware(mut self, middleware: impl Middleware) -> Self {
        self.middleware.push(Arc::new(middleware));
        self
    }

    /// Set a request observer for real-time phase transition callbacks.
    ///
    /// The observer fires at each connection phase (pool checkout, DNS, TCP,
    /// TLS, request sent, response received) with monotonic timestamps and
    /// diagnostic data. Useful for load testing frameworks, detailed performance
    /// tracing, and custom instrumentation.
    ///
    /// Only one observer is supported per engine. Setting a new observer
    /// replaces the previous one.
    pub fn request_observer(mut self, observer: impl crate::observer::RequestObserver) -> Self {
        self.observer = Some(Arc::new(observer));
        self
    }

    /// Set a rate limiter to throttle outgoing requests.
    pub fn rate_limiter(mut self, limiter: crate::throttle::RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Set a maximum number of requests per second for outgoing requests.
    ///
    /// Internally constructs a [`RateLimiter`](crate::RateLimiter) with a 1-second window.
    /// For full control over the token-bucket parameters, use
    /// [`rate_limiter`](Self::rate_limiter) directly.
    pub fn max_requests_per_sec(mut self, n: u64) -> Self {
        self.rate_limiter = Some(crate::throttle::RateLimiter::new(
            n,
            std::time::Duration::from_secs(1),
        ));
        self
    }

    /// Set a bandwidth limiter to throttle download throughput (bytes per second).
    pub fn max_download_speed(mut self, bytes_per_sec: u64) -> Self {
        self.bandwidth_limiter = Some(crate::bandwidth::BandwidthLimiter::new(bytes_per_sec));
        self
    }

    /// Enable HTTP Digest Authentication with the given credentials.
    ///
    /// When a server responds with `401 Unauthorized` and a `WWW-Authenticate: Digest`
    /// challenge, the client will automatically retry the request with digest credentials.
    pub fn digest_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.digest_auth = Some(crate::digest_auth::DigestAuth::new(
            username.into(),
            password.into(),
        ));
        self
    }

    /// Enable HTTP response caching with the given cache instance.
    pub fn cache(mut self, cache: HttpCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Enable HSTS (HTTP Strict Transport Security) auto-upgrade.
    ///
    /// When enabled, `http://` URLs are automatically upgraded to `https://`
    /// for hosts that have sent a `Strict-Transport-Security` header.
    pub fn hsts(mut self, store: crate::hsts::HstsStore) -> Self {
        self.hsts = Some(store);
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
        L: tower_layer::Layer<crate::connector::ConnectorService<C>>,
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
        self.tower_connector = Some(crate::connector::apply_layer(self.connector.clone(), layer));
        self
    }

    #[cfg(feature = "rustls")]
    /// Set the TLS connector for HTTPS.
    pub fn tls(mut self, connector: crate::tls::RustlsConnector) -> Self {
        self.tls = Some(Arc::new(connector));
        self
    }

    #[cfg(feature = "rustls")]
    /// Set the minimum TLS version to allow (default: TLS 1.2).
    pub fn min_tls_version(mut self, version: TlsVersion) -> Self {
        self.min_tls_version = Some(version);
        self
    }

    #[cfg(feature = "rustls")]
    /// Set the maximum TLS version to allow (default: TLS 1.3).
    pub fn max_tls_version(mut self, version: TlsVersion) -> Self {
        self.max_tls_version = Some(version);
        self
    }

    #[cfg(feature = "rustls")]
    /// Control whether to send the SNI extension (default: true).
    pub fn tls_sni(mut self, enable: bool) -> Self {
        self.tls_sni = Some(enable);
        self
    }

    #[cfg(feature = "rustls")]
    /// Accept invalid TLS certificates (INSECURE — for testing/dev only).
    pub fn danger_accept_invalid_certs(self) -> Self {
        self.tls(crate::tls::RustlsConnector::danger_accept_invalid_certs())
    }

    #[cfg(feature = "rustls")]
    /// Add custom trusted CA certificates alongside the default WebPKI roots.
    pub fn add_root_certificates(mut self, certs: &[crate::tls::Certificate]) -> Self {
        self.extra_root_certs.extend(
            certs
                .iter()
                .map(|c| crate::tls::Certificate { der: c.der.clone() }),
        );
        self
    }

    #[cfg(feature = "rustls")]
    /// Set a client identity (certificate + key) for mutual TLS authentication.
    pub fn identity(mut self, identity: crate::tls::Identity) -> Self {
        self.client_identity = Some(identity);
        self
    }

    #[cfg(feature = "rustls")]
    /// Add certificate revocation lists for TLS revocation checking.
    pub fn add_crls(
        mut self,
        crls: impl IntoIterator<Item = crate::tls::CertificateRevocationList>,
    ) -> Self {
        self.crls.extend(crls);
        self
    }

    #[cfg(feature = "rustls")]
    /// Accept TLS certificates with mismatched hostnames (INSECURE — testing only).
    ///
    /// This is separate from `danger_accept_invalid_certs`: the certificate chain
    /// is still validated, but hostname verification is skipped.
    pub fn danger_accept_invalid_hostnames(mut self, accept: bool) -> Self {
        self.danger_accept_invalid_hostnames = accept;
        self
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Enable or disable HTTP/3 for all HTTPS requests.
    pub fn http3(mut self, enable: bool) -> Self {
        if enable {
            self = self.ensure_h3_endpoint();
            self.prefer_h3 = true;
        } else {
            self.h3_endpoint = None;
            self.prefer_h3 = false;
        }
        self
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Enable automatic HTTP/3 upgrade via Alt-Svc headers.
    pub fn alt_svc_h3(mut self, enable: bool) -> Self {
        if enable {
            self = self.ensure_h3_endpoint();
        } else if !self.prefer_h3 {
            self.h3_endpoint = None;
        }
        self
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
    fn ensure_h3_endpoint(mut self) -> Self {
        if self.h3_endpoint.is_none() {
            let tls_config = self
                .tls
                .as_ref()
                .expect("HTTP/3 requires a TLS connector — call .tls() before .http3(true)")
                .config()
                .clone();
            let endpoint = crate::h3_transport::build_quinn_endpoint(
                tls_config,
                self.local_address,
                self.h3_zero_rtt,
            )
            .expect("failed to build QUIC endpoint");
            self.h3_endpoint = Some(endpoint);
        }
        self
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

    /// Build the configured [`HttpEngine`].
    pub fn build(self) -> HttpEngine<R, C> {
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
                        root_store
                            .add(cert.der.clone())
                            .expect("invalid extra root certificate");
                    }
                    let crls: Vec<_> = self.crls.into_iter().map(|c| c.der).collect();
                    let identity = self.client_identity.map(|id| (id.certs, id.key));
                    Some(Arc::new(
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
                self.tls
            };

            if needs_sni_update {
                let c = connector.get_or_insert_with(|| {
                    Arc::new(crate::tls::RustlsConnector::with_webpki_roots())
                });
                Arc::make_mut(c).config_mut().enable_sni = false;
            }

            connector
        };

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
            observer: self.observer,
            #[cfg(feature = "tower")]
            tower_connector: self.tower_connector,
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
            _phantom: PhantomData,
        }
    }
}
