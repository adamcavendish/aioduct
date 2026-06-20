use std::net::IpAddr;
use std::num::NonZeroUsize;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use http::header::{HeaderMap, HeaderValue, USER_AGENT};

use crate::error::BuilderError;
use crate::http2::Http2Config;
use crate::middleware::Middleware;
use crate::proxy::{ProxyConfig, ProxySettings};
use crate::redirect::RedirectPolicy;
use crate::retry::RetryConfig;
use crate::runtime::Resolve;
#[cfg(feature = "rustls")]
use crate::tls::TlsVersion;

use super::builder::HttpEngineBuilder;

impl<R, C> HttpEngineBuilder<R, C> {
    /// Set a base URL that relative request URLs resolve against.
    ///
    /// When set, request URLs are resolved against this base per RFC 3986:
    /// - A relative reference (`"users"`, `"/users"`, `"?q=1"`) resolves against
    ///   the base. A trailing slash matters: base `"https://api.example.com/v1/"`
    ///   joined with `"users"` yields `"https://api.example.com/v1/users"`, while
    ///   base `"https://api.example.com/v1"` joined with `"users"` yields
    ///   `"https://api.example.com/users"`.
    /// - An absolute URL (with scheme and authority) overrides the base entirely.
    ///
    /// Returns an error if the base URL cannot be parsed, is not `http`/`https`,
    /// or has no authority (host).
    pub fn base_url(mut self, base: &str) -> Result<Self, crate::error::Error> {
        let parsed =
            url::Url::parse(base).map_err(|e| crate::error::Error::InvalidUrl(format!("{e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(crate::error::Error::InvalidUrl(format!(
                "base_url scheme must be http or https, got `{}`",
                parsed.scheme()
            )));
        }
        if parsed.host_str().is_none_or(|h| h.is_empty()) {
            return Err(crate::error::Error::InvalidUrl(
                "base_url must include a host".into(),
            ));
        }
        self.base_url = Some(Arc::new(parsed));
        Ok(self)
    }

    /// Set the idle connection timeout (default: 90s).
    pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.pool_idle_timeout = timeout;
        self
    }

    /// Set the maximum lifetime for pooled connections.
    ///
    /// Connections older than this are not reused once they return idle.
    /// In-flight requests are not interrupted.
    pub fn pool_max_lifetime(mut self, lifetime: Duration) -> Self {
        self.pool_max_lifetime = Some(lifetime);
        self
    }

    /// Set the max idle connections per host (default: 10).
    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.pool_max_idle_per_host = max;
        self
    }

    /// Set the max active connections per host.
    ///
    /// When `max` is 0, the cap is disabled (unlimited). Limits the number of
    /// concurrently checked-out pool handles and in-progress fresh connection
    /// attempts for the same pool key.
    ///
    /// # Panics
    ///
    /// Does **not** panic on 0 — 0 is treated as unlimited.
    pub fn pool_max_active_per_host(mut self, max: usize) -> Self {
        self.pool_max_active_per_host = NonZeroUsize::new(max);
        self
    }

    /// Set the maximum active multiplexed streams per HTTP/2 or HTTP/3 connection.
    ///
    /// The default is unlimited. This does not affect HTTP/1.1 connections.
    ///
    /// # Panics
    ///
    /// Panics if `max` is 0.
    pub fn pool_max_active_streams_per_connection(mut self, max: usize) -> Self {
        assert!(
            max > 0,
            "pool_max_active_streams_per_connection must be greater than 0"
        );
        self.pool_max_active_streams_per_connection = NonZeroUsize::new(max);
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
    /// request fails with [`Error::ReadTimeout`](crate::Error::ReadTimeout).
    ///
    /// This is the client default and can be overridden per request with
    /// [`RequestBuilderSend::read_timeout`](crate::request::RequestBuilderSend::read_timeout).
    ///
    /// Use [`timeout()`](Self::timeout) to bound one request attempt until
    /// `send()` returns, or [`connect_timeout()`](Self::connect_timeout) for
    /// the TCP + TLS handshake phase.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = Some(timeout);
        self
    }

    /// Set a timeout for writing (uploading) the request body.
    ///
    /// This applies **only to request body uploads**, not to waiting for
    /// response headers or reading the response body. If the HTTP engine
    /// cannot accept more body data within this duration (e.g., flow-control
    /// backpressure from a slow server), the request fails with
    /// [`Error::WriteTimeout`](crate::Error::WriteTimeout).
    ///
    /// Use [`timeout()`](Self::timeout) to bound one request attempt until
    /// `send()` returns, or [`read_timeout()`](Self::read_timeout) for the
    /// response body.
    pub fn write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = Some(timeout);
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

    /// Restrict or prefer the IP address family for resolved connections.
    ///
    /// Applied to resolver results before Happy Eyeballs racing. The `*Only`
    /// variants drop the other family (the connection fails if none remain);
    /// the `Prefer*` variants reorder so the preferred family is tried first
    /// while keeping the other as fallback. IP-literal request URLs and
    /// [`force_addr`](crate::request::RequestBuilderSend::force_addr) bypass
    /// this filter (the address is the caller's deliberate choice). Defaults to
    /// [`AddressFamily::Any`](crate::AddressFamily::Any).
    ///
    /// For HTTP/3, the QUIC endpoint's bound family also constrains reachability:
    /// an IPv4-bound endpoint cannot reach IPv6 peers, so `Ipv6Only` over such an
    /// endpoint yields no usable address.
    pub fn address_family(mut self, family: crate::address_family::AddressFamily) -> Self {
        self.address_family = family;
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
    pub fn user_agent(mut self, value: impl AsRef<str>) -> Self {
        match HeaderValue::from_str(value.as_ref()) {
            Ok(val) => {
                self.default_headers.insert(USER_AGENT, val);
            }
            Err(e) => BuilderError::set_once(
                &mut self.builder_error,
                BuilderError::invalid_header(format!("invalid user-agent header value: {e}")),
            ),
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

    /// Disable automatic response body decompression.
    pub fn no_decompression(mut self) -> Self {
        self.accept_encoding = crate::decompress::AcceptEncoding::none();
        self
    }

    /// Set the maximum decompressed body size in bytes.
    ///
    /// When set, decompressed responses exceeding this limit produce an error
    /// instead of allocating unbounded memory. `None` (the default) means
    /// unlimited.
    pub fn max_decoded_size(mut self, max: impl Into<Option<u64>>) -> Self {
        self.accept_encoding.max_decoded_size = max.into();
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
    pub fn cookie_jar(mut self, jar: crate::cookie::CookieJar) -> Self {
        self.cookie_jar = Some(jar);
        self
    }

    /// Route requests through an HTTP proxy (used for both HTTP and HTTPS targets).
    pub fn proxy(mut self, config: ProxyConfig) -> Self {
        self.proxy = Some(ProxySettings::all(config));
        self
    }

    /// Route all requests through a chain of proxies.
    ///
    /// Each proxy is reached through the previous one via HTTP CONNECT tunneling.
    /// Currently supports up to 2 hops.
    ///
    /// When both a proxy chain and a single proxy are configured, the chain
    /// takes priority.
    pub fn proxy_chain(mut self, chain: crate::proxy::ProxyChain) -> Self {
        self.proxy_chain = Some(chain);
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
    /// ```no_run
    /// # #[cfg(feature = "tokio")]
    /// # {
    /// # use aioduct::{HttpEngineSend, runtime::TokioRuntime};
    /// # use aioduct::runtime::tokio_rt::TcpConnector;
    /// let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
    ///     .resolve("example.com", "127.0.0.1:8080".parse().unwrap())
    ///     .build().unwrap();
    /// # }
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
    /// # use aioduct::{HttpEngineSend, runtime::TokioRuntime};
    /// let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
    ///     .dns_over_https("1.1.1.1".parse().unwrap(), "cloudflare-dns.com")
    ///     .build().unwrap();
    /// ```
    #[cfg(feature = "doh")]
    pub fn dns_over_https(
        self,
        server_ip: std::net::IpAddr,
        server_name: &str,
    ) -> Result<Self, crate::error::Error> {
        use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
        let ns = NameServerConfig::https(server_ip, std::sync::Arc::from(server_name), None);
        let config = ResolverConfig::from_parts(None, vec![], vec![ns]);
        let resolver = crate::HickoryResolver::from_config(config, ResolverOpts::default())
            .map_err(crate::error::Error::Io)?;
        Ok(self.resolver(resolver))
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
    /// # use aioduct::{HttpEngineSend, runtime::TokioRuntime};
    /// let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
    ///     .dns_over_tls("1.1.1.1".parse().unwrap(), "cloudflare-dns.com")
    ///     .build().unwrap();
    /// ```
    #[cfg(feature = "dot")]
    pub fn dns_over_tls(
        self,
        server_ip: std::net::IpAddr,
        server_name: &str,
    ) -> Result<Self, crate::error::Error> {
        use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
        let ns = NameServerConfig::tls(server_ip, std::sync::Arc::from(server_name));
        let config = ResolverConfig::from_parts(None, vec![], vec![ns]);
        let resolver = crate::HickoryResolver::from_config(config, ResolverOpts::default())
            .map_err(crate::error::Error::Io)?;
        Ok(self.resolver(resolver))
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

    /// Mark a header as sensitive so it is stripped on cross-origin redirects.
    ///
    /// The standard headers `Authorization`, `Cookie`, and `Proxy-Authorization`
    /// are always stripped. Use this for custom headers like `X-Api-Key`.
    pub fn sensitive_header(mut self, name: http::header::HeaderName) -> Self {
        self.sensitive_headers.insert(name);
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
    pub fn cache(mut self, cache: crate::cache::HttpCache) -> Self {
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
}
