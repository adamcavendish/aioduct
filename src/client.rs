use std::future::Future;
use std::marker::PhantomData;
use std::net::IpAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{
    AUTHORIZATION, COOKIE, HOST, HeaderMap, HeaderValue, LOCATION, PROXY_AUTHORIZATION, REFERER,
    USER_AGENT,
};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;

use crate::body::RequestBody;
use crate::cache::HttpCache;
use crate::cookie::CookieJar;
use crate::error::{Error, HyperBody, Result};
use crate::http2::Http2Config;
use crate::middleware::{Middleware, MiddlewareStack};
use crate::pool::{ConnectionPool, HttpConnection, PooledConnection};
use crate::proxy::{ProxyConfig, ProxySettings};
use crate::redirect::{RedirectAction, RedirectPolicy};
use crate::request::RequestBuilder;
use crate::response::Response;
use crate::retry::RetryConfig;
use crate::runtime::{Resolve, Runtime};
#[cfg(feature = "rustls")]
use crate::tls::TlsVersion;

const DEFAULT_USER_AGENT: &str = concat!("aioduct/", env!("CARGO_PKG_VERSION"));

/// HTTP client with connection pooling, TLS, and automatic redirect handling.
pub struct Client<R: Runtime> {
    pool: ConnectionPool<R>,
    redirect_policy: RedirectPolicy,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    tcp_keepalive: Option<Duration>,
    tcp_keepalive_interval: Option<Duration>,
    tcp_keepalive_retries: Option<u32>,
    local_address: Option<IpAddr>,
    #[cfg(target_os = "linux")]
    interface: Option<String>,
    #[cfg(unix)]
    unix_socket: Option<PathBuf>,
    https_only: bool,
    referer: bool,
    no_connection_reuse: bool,
    http2_prior_knowledge: bool,
    accept_encoding: crate::decompress::AcceptEncoding,
    default_headers: HeaderMap,
    retry: Option<RetryConfig>,
    cookie_jar: Option<CookieJar>,
    proxy: Option<ProxySettings>,
    resolver: Option<Arc<dyn Resolve>>,
    http2: Option<Http2Config>,
    middleware: MiddlewareStack,
    rate_limiter: Option<crate::throttle::RateLimiter>,
    cache: Option<HttpCache>,
    #[cfg(feature = "tower")]
    connector: Option<crate::connector::LayeredConnector<R>>,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    #[cfg(feature = "http3")]
    h3_endpoint: Option<quinn::Endpoint>,
    #[cfg(feature = "http3")]
    prefer_h3: bool,
    #[cfg(feature = "http3")]
    alt_svc_cache: crate::alt_svc::AltSvcCache,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> Clone for Client<R> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            redirect_policy: self.redirect_policy.clone(),
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            tcp_keepalive: self.tcp_keepalive,
            tcp_keepalive_interval: self.tcp_keepalive_interval,
            tcp_keepalive_retries: self.tcp_keepalive_retries,
            local_address: self.local_address,
            #[cfg(target_os = "linux")]
            interface: self.interface.clone(),
            #[cfg(unix)]
            unix_socket: self.unix_socket.clone(),
            https_only: self.https_only,
            referer: self.referer,
            no_connection_reuse: self.no_connection_reuse,
            http2_prior_knowledge: self.http2_prior_knowledge,
            accept_encoding: self.accept_encoding.clone(),
            default_headers: self.default_headers.clone(),
            retry: self.retry.clone(),
            cookie_jar: self.cookie_jar.clone(),
            proxy: self.proxy.clone(),
            resolver: self.resolver.clone(),
            http2: self.http2.clone(),
            middleware: self.middleware.clone(),
            rate_limiter: self.rate_limiter.clone(),
            cache: self.cache.clone(),
            #[cfg(feature = "tower")]
            connector: self.connector.clone(),
            #[cfg(feature = "rustls")]
            tls: self.tls.clone(),
            #[cfg(feature = "http3")]
            h3_endpoint: self.h3_endpoint.clone(),
            #[cfg(feature = "http3")]
            prefer_h3: self.prefer_h3,
            #[cfg(feature = "http3")]
            alt_svc_cache: self.alt_svc_cache.clone(),
            _runtime: PhantomData,
        }
    }
}

/// Builder for configuring a [`Client`].
pub struct ClientBuilder<R: Runtime> {
    pool_idle_timeout: Duration,
    pool_max_idle_per_host: usize,
    no_connection_reuse: bool,
    http2_prior_knowledge: bool,
    redirect_policy: RedirectPolicy,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
    tcp_keepalive: Option<Duration>,
    tcp_keepalive_interval: Option<Duration>,
    tcp_keepalive_retries: Option<u32>,
    local_address: Option<IpAddr>,
    #[cfg(target_os = "linux")]
    interface: Option<String>,
    #[cfg(unix)]
    unix_socket: Option<PathBuf>,
    https_only: bool,
    referer: bool,
    accept_encoding: crate::decompress::AcceptEncoding,
    default_headers: HeaderMap,
    retry: Option<RetryConfig>,
    cookie_jar: Option<CookieJar>,
    proxy: Option<ProxySettings>,
    resolver: Option<Arc<dyn Resolve>>,
    http2: Option<Http2Config>,
    middleware: MiddlewareStack,
    rate_limiter: Option<crate::throttle::RateLimiter>,
    cache: Option<HttpCache>,
    #[cfg(feature = "tower")]
    connector: Option<crate::connector::LayeredConnector<R>>,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
    #[cfg(feature = "rustls")]
    min_tls_version: Option<TlsVersion>,
    #[cfg(feature = "rustls")]
    max_tls_version: Option<TlsVersion>,
    #[cfg(feature = "rustls")]
    tls_sni: Option<bool>,
    #[cfg(feature = "rustls")]
    extra_root_certs: Vec<crate::tls::Certificate>,
    #[cfg(feature = "rustls")]
    client_identity: Option<crate::tls::Identity>,
    #[cfg(feature = "rustls")]
    crls: Vec<crate::tls::CertificateRevocationList>,
    #[cfg(feature = "rustls")]
    danger_accept_invalid_hostnames: bool,
    #[cfg(feature = "http3")]
    h3_endpoint: Option<quinn::Endpoint>,
    #[cfg(feature = "http3")]
    prefer_h3: bool,
    _runtime: PhantomData<R>,
}

impl<R: Runtime> Default for ClientBuilder<R> {
    fn default() -> Self {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));

        Self {
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 10,
            no_connection_reuse: false,
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
            http2: None,
            middleware: MiddlewareStack::new(),
            rate_limiter: None,
            cache: None,
            #[cfg(feature = "tower")]
            connector: None,
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
            #[cfg(feature = "http3")]
            h3_endpoint: None,
            #[cfg(feature = "http3")]
            prefer_h3: false,
            _runtime: PhantomData,
        }
    }
}

impl<R: Runtime> ClientBuilder<R> {
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

    /// Set a timeout between body data chunks (read timeout).
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

    /// Configure HTTP/2 connection parameters (window sizes, keepalive, frame size).
    pub fn http2(mut self, config: Http2Config) -> Self {
        self.http2 = Some(config);
        self
    }

    /// Add a middleware layer that can inspect or modify requests and responses.
    pub fn middleware(mut self, middleware: impl Middleware) -> Self {
        self.middleware.push(Arc::new(middleware));
        self
    }

    /// Set a rate limiter to throttle outgoing requests.
    pub fn rate_limiter(mut self, limiter: crate::throttle::RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Enable HTTP response caching with the given cache instance.
    pub fn cache(mut self, cache: HttpCache) -> Self {
        self.cache = Some(cache);
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
        L: tower_layer::Layer<crate::connector::RuntimeConnector<R>>,
        L::Service: tower_service::Service<
                crate::connector::ConnectInfo,
                Response = R::TcpStream,
                Error = std::io::Error,
            > + Send
            + Sync
            + Clone
            + 'static,
        <L::Service as tower_service::Service<crate::connector::ConnectInfo>>::Future:
            Send + 'static,
    {
        self.connector = Some(crate::connector::apply_layer(layer));
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

    #[cfg(feature = "http3")]
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

    #[cfg(feature = "http3")]
    /// Enable automatic HTTP/3 upgrade via Alt-Svc headers.
    pub fn alt_svc_h3(mut self, enable: bool) -> Self {
        if enable {
            self = self.ensure_h3_endpoint();
        } else if !self.prefer_h3 {
            self.h3_endpoint = None;
        }
        self
    }

    #[cfg(feature = "http3")]
    fn ensure_h3_endpoint(mut self) -> Self {
        if self.h3_endpoint.is_none() {
            let tls_config = self
                .tls
                .as_ref()
                .expect("HTTP/3 requires a TLS connector — call .tls() before .http3(true)")
                .config()
                .clone();
            let endpoint = crate::h3_transport::build_quinn_endpoint(tls_config)
                .expect("failed to build QUIC endpoint");
            self.h3_endpoint = Some(endpoint);
        }
        self
    }

    /// Build the configured [`Client`].
    pub fn build(self) -> Client<R> {
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
            let needs_configured =
                has_crls || self.danger_accept_invalid_hostnames;
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
                        let _ = root_store.add(cert.der.clone());
                    }
                    let crls: Vec<_> = self.crls.into_iter().map(|c| c.der).collect();
                    let identity = self.client_identity.map(|id| (id.certs, id.key));
                    crate::tls::RustlsConnector::build_configured(
                        root_store,
                        &versions,
                        crls,
                        self.danger_accept_invalid_hostnames,
                        identity,
                    )
                    .ok()
                    .map(Arc::new)
                    .or(self.tls)
                } else if let Some(identity) = self.client_identity {
                    crate::tls::RustlsConnector::with_identity_versioned(
                        &self.extra_root_certs,
                        identity,
                        &versions,
                    )
                    .ok()
                    .map(Arc::new)
                    .or(self.tls)
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
                if let Some(ref mut c) = connector {
                    Arc::make_mut(c).config_mut().enable_sni = false;
                }
            }

            connector
        };

        Client {
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
            http2_prior_knowledge: self.http2_prior_knowledge,
            accept_encoding: self.accept_encoding,
            default_headers: self.default_headers,
            retry: self.retry,
            cookie_jar: self.cookie_jar,
            proxy: self.proxy,
            resolver: self.resolver,
            http2: self.http2,
            middleware: self.middleware,
            rate_limiter: self.rate_limiter,
            cache: self.cache,
            #[cfg(feature = "tower")]
            connector: self.connector,
            #[cfg(feature = "rustls")]
            tls,
            #[cfg(feature = "http3")]
            h3_endpoint: self.h3_endpoint,
            #[cfg(feature = "http3")]
            prefer_h3: self.prefer_h3,
            #[cfg(feature = "http3")]
            alt_svc_cache: crate::alt_svc::AltSvcCache::new(),
            _runtime: PhantomData,
        }
    }
}

impl<R: Runtime> Default for Client<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Runtime> Client<R> {
    /// Create a new [`ClientBuilder`] with default settings.
    pub fn builder() -> ClientBuilder<R> {
        ClientBuilder::default()
    }

    /// Create a new client with default settings.
    pub fn new() -> Self {
        Self::builder().build()
    }

    #[cfg(feature = "rustls")]
    /// Create a client with rustls TLS using WebPKI root certificates.
    pub fn with_rustls() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build()
    }

    #[cfg(feature = "rustls-native-roots")]
    /// Create a client with rustls TLS using the system's native root certificates.
    pub fn with_native_roots() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_native_roots())
            .build()
    }

    #[cfg(feature = "http3")]
    /// Create a client configured for HTTP/3 with rustls.
    pub fn with_http3() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .build()
    }

    #[cfg(feature = "http3")]
    /// Create a client that upgrades to HTTP/3 via Alt-Svc discovery.
    pub fn with_alt_svc_h3() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .alt_svc_h3(true)
            .build()
    }

    /// Start a GET request to the given URL.
    pub fn get(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::GET, uri))
    }

    /// Start a HEAD request to the given URL.
    pub fn head(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::HEAD, uri))
    }

    /// Start a POST request to the given URL.
    pub fn post(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::POST, uri))
    }

    /// Start a PUT request to the given URL.
    pub fn put(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::PUT, uri))
    }

    /// Start a PATCH request to the given URL.
    pub fn patch(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::PATCH, uri))
    }

    /// Start a DELETE request to the given URL.
    pub fn delete(&self, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::DELETE, uri))
    }

    /// Start a request with the given method and URL.
    pub fn request(&self, method: Method, uri: &str) -> Result<RequestBuilder<'_, R>> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, method, uri))
    }

    /// Start a parallel chunk download for the given URL.
    pub fn chunk_download(&self, url: &str) -> crate::chunk_download::ChunkDownload<R> {
        crate::chunk_download::ChunkDownload::new(self.clone(), url.to_owned())
    }

    pub(crate) fn default_timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub(crate) fn default_retry(&self) -> Option<&RetryConfig> {
        self.retry.as_ref()
    }

    pub(crate) async fn execute(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<RequestBody>,
        version: Option<http::Version>,
    ) -> Result<Response> {
        if self.https_only && original_uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Err(Error::Other(
                format!(
                    "https_only is enabled but URL scheme is {:?}",
                    original_uri.scheme_str().unwrap_or("none")
                )
                .into(),
            ));
        }

        let mut current_uri = original_uri;
        let mut current_method = method;
        let mut current_body = body;
        let mut current_headers = headers;

        for (name, value) in &self.default_headers {
            if !current_headers.contains_key(name) {
                current_headers.insert(name, value.clone());
            }
        }

        crate::decompress::set_accept_encoding(&mut current_headers, &self.accept_encoding);

        for _ in 0..=self.redirect_policy.max_redirects() {
            if let Some(jar) = &self.cookie_jar {
                if let Some(authority) = current_uri.authority() {
                    let is_secure = current_uri.scheme() == Some(&http::uri::Scheme::HTTPS);
                    jar.apply_to_request(authority.host(), is_secure, &mut current_headers);
                }
            }

            let (req_body, body_for_redirect) = match current_body.take() {
                Some(RequestBody::Buffered(b)) => {
                    let body_clone = RequestBody::Buffered(b.clone());
                    (RequestBody::Buffered(b).into_hyper_body(), Some(body_clone))
                }
                Some(rb @ RequestBody::Streaming(_)) => (rb.into_hyper_body(), None),
                None => {
                    let empty: HyperBody = http_body_util::Full::new(Bytes::new())
                        .map_err(|never| match never {})
                        .boxed();
                    (empty, None)
                }
            };

            if !current_headers.contains_key(HOST) {
                if let Some(authority) = current_uri.authority() {
                    if let Ok(host_value) = authority.as_str().parse() {
                        current_headers.insert(HOST, host_value);
                    }
                }
            }

            // Cache lookup: return fresh cached response or prepare conditional headers
            let cache_state = if let Some(ref cache) = self.cache {
                match cache.lookup(&current_method, &current_uri) {
                    crate::cache::CacheLookup::Fresh(cached) => {
                        let http_resp = cached.into_http_response();
                        return Ok(Response::new(http_resp, current_uri));
                    }
                    crate::cache::CacheLookup::Stale { validators, cached } => {
                        validators.apply_to_request(&mut current_headers);
                        Some(cached)
                    }
                    crate::cache::CacheLookup::Miss => None,
                }
            } else {
                None
            };

            let path_and_query = current_uri
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/");
            let req_uri: Uri = path_and_query
                .parse()
                .map_err(|e| Error::Other(Box::new(e)))?;

            let mut builder = http::Request::builder()
                .method(current_method.clone())
                .uri(req_uri);

            if let Some(ver) = version {
                builder = builder.version(ver);
            }

            for (name, value) in &current_headers {
                builder = builder.header(name, value);
            }

            let mut request = builder.body(req_body)?;

            if !self.middleware.is_empty() {
                self.middleware.apply_request(&mut request, &current_uri);
            }

            let resp = self.execute_single(request, &current_uri).await?;

            // Handle 304 Not Modified: reuse cached response
            if resp.status() == StatusCode::NOT_MODIFIED {
                if let Some(cached) = cache_state {
                    let http_resp = cached.into_http_response();
                    return Ok(Response::new(http_resp, current_uri));
                }
            }

            // Invalidate cache on unsafe methods
            if let Some(ref cache) = self.cache {
                cache.invalidate(&current_method, &current_uri);
            }

            if let Some(jar) = &self.cookie_jar {
                if let Some(authority) = current_uri.authority() {
                    jar.store_from_response(authority.host(), resp.headers());
                }
            }

            if !resp.status().is_redirection()
                || matches!(self.redirect_policy, RedirectPolicy::None)
            {
                #[cfg(feature = "http3")]
                if self.h3_endpoint.is_some() {
                    self.cache_alt_svc(&current_uri, resp.headers());
                }
                let mut resp = resp;
                if !self.middleware.is_empty() {
                    self.middleware
                        .apply_response(resp.inner_mut(), &current_uri);
                }

                // Store cacheable responses in the HTTP cache
                if let Some(ref cache) = self.cache {
                    let status = resp.status();
                    let headers = resp.headers().clone();
                    if crate::cache::is_response_cacheable(status, &headers) {
                        let body_bytes = resp.bytes().await?;
                        cache.store(&current_method, &current_uri, status, &headers, &body_bytes);
                        let cached_resp = http::Response::builder()
                            .status(status);
                        let cached_resp = {
                            let mut builder = cached_resp;
                            for (name, value) in &headers {
                                builder = builder.header(name, value);
                            }
                            builder.body(
                                http_body_util::Full::new(body_bytes)
                                    .map_err(|never| match never {})
                                    .boxed(),
                            )?
                        };
                        return Ok(Response::new(cached_resp, current_uri));
                    }
                }

                let resp = resp.decompress(&self.accept_encoding);
                let resp = if let Some(read_timeout) = self.read_timeout {
                    resp.apply_read_timeout::<R>(read_timeout)
                } else {
                    resp
                };
                return Ok(resp);
            }

            let status = resp.status();
            let location = resp
                .headers()
                .get(LOCATION)
                .ok_or_else(|| Error::Other("redirect without Location header".into()))?
                .to_str()
                .map_err(|e| Error::Other(Box::new(e)))?
                .to_owned();

            let next_uri = resolve_redirect(&current_uri, &location)?;

            if self
                .redirect_policy
                .check(&current_uri, &next_uri, status, &current_method)
                == RedirectAction::Stop
            {
                let _ = resp.bytes().await;
                return Ok(Response::new(
                    http::Response::builder()
                        .status(status)
                        .header(LOCATION, location)
                        .body(
                            http_body_util::Full::new(Bytes::new())
                                .map_err(|never| match never {})
                                .boxed(),
                        )?,
                    current_uri.clone(),
                ));
            }

            let _ = resp.bytes().await;

            match status {
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER => {
                    current_method = Method::GET;
                    current_body = None;
                }
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                    current_body = body_for_redirect;
                }
                _ => return Err(Error::Other("unexpected redirect status".into())),
            }

            // Update Host header for the new target
            if let Some(authority) = next_uri.authority() {
                if let Ok(host_value) = authority.as_str().parse() {
                    current_headers.insert(HOST, host_value);
                }
            }

            // Strip sensitive headers on cross-origin redirect
            let same_origin = current_uri.authority() == next_uri.authority()
                && current_uri.scheme() == next_uri.scheme();
            if !same_origin {
                current_headers.remove(AUTHORIZATION);
                current_headers.remove(COOKIE);
                current_headers.remove(PROXY_AUTHORIZATION);
            }

            if self.referer {
                if let Ok(val) = HeaderValue::from_str(&current_uri.to_string()) {
                    current_headers.insert(REFERER, val);
                }
            }

            current_uri = next_uri;
        }

        Err(Error::Other(
            format!(
                "too many redirects (max {})",
                self.redirect_policy.max_redirects()
            )
            .into(),
        ))
    }

    async fn execute_single(
        &self,
        request: http::Request<HyperBody>,
        original_uri: &Uri,
    ) -> Result<Response> {
        if let Some(ref limiter) = self.rate_limiter {
            while !limiter.try_acquire() {
                let wait = limiter.wait_duration();
                R::sleep(wait).await;
            }
        }

        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        let pool_key = crate::pool::PoolKey::new(scheme.clone(), authority.clone());

        if !self.no_connection_reuse {
            if let Some(mut conn) = self.pool.checkout(&pool_key) {
                let mut resp =
                    Self::send_on_connection(&mut conn, request, original_uri.clone()).await?;
                resp.set_remote_addr(conn.remote_addr);
                resp.set_tls_info(conn.tls_info.clone());
                if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                    self.pool.checkin(pool_key, conn);
                }
                return Ok(resp);
            }
        }

        #[cfg(feature = "http3")]
        if is_https {
            if let Some(endpoint) = &self.h3_endpoint {
                let use_h3 = self.prefer_h3 || self.alt_svc_cache.lookup_h3(authority).is_some();
                if use_h3 {
                    let default_port = 443u16;
                    let (h3_host, h3_port) = self
                        .alt_svc_cache
                        .lookup_h3(authority)
                        .unwrap_or_else(|| (None, authority.port_u16().unwrap_or(default_port)));
                    let connect_host = h3_host.as_deref().unwrap_or(authority.host());
                    let addr = self.resolve_authority_raw(connect_host, h3_port).await?;
                    let sni_host = authority.host().to_owned();
                    let quinn_conn = endpoint
                        .connect(addr, &sni_host)
                        .map_err(|e| Error::Other(Box::new(e)))?
                        .await
                        .map_err(|e| Error::Other(Box::new(e)))?;
                    let mut pooled = crate::h3_transport::connect_h3::<R>(quinn_conn).await?;
                    pooled.remote_addr = Some(addr);
                    let mut resp =
                        Self::send_on_connection(&mut pooled, request, original_uri.clone())
                            .await?;
                    resp.set_remote_addr(pooled.remote_addr);
                    resp.set_tls_info(pooled.tls_info.clone());
                    if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                        self.pool.checkin(pool_key, pooled);
                    }
                    return Ok(resp);
                }
            }
        }

        let proxy = self
            .proxy
            .as_ref()
            .and_then(|settings| settings.proxy_for(original_uri));

        #[cfg(unix)]
        let unix_socket = self.unix_socket.as_ref();
        #[cfg(not(unix))]
        let unix_socket: Option<&std::path::PathBuf> = None;

        let mut pooled = if let Some(unix_path) = unix_socket {
            let _ = &proxy; // suppress unused warning when unix_socket is set
            #[cfg(unix)]
            {
                let connect_fut = async {
                    let unix_stream = R::connect_unix(unix_path)
                        .await
                        .map_err(Error::Io)?;
                    self.connect_plaintext(unix_stream).await
                };
                match self.connect_timeout {
                    Some(duration) => {
                        crate::timeout::Timeout::WithTimeout {
                            future: connect_fut,
                            sleep: R::sleep(duration),
                        }
                        .await?
                    }
                    None => connect_fut.await?,
                }
            }
            #[cfg(not(unix))]
            unreachable!()
        } else if let Some(ref proxy) = proxy {
            self.connect_via_proxy(proxy, authority, is_https).await?
        } else {
            let default_port = if is_https { 443 } else { 80 };
            let addr = self.resolve_authority(authority, default_port).await?;

            let tcp_keepalive = self.tcp_keepalive;
            let tcp_keepalive_interval = self.tcp_keepalive_interval;
            let tcp_keepalive_retries = self.tcp_keepalive_retries;
            let local_address = self.local_address;
            #[cfg(target_os = "linux")]
            let interface = self.interface.as_deref();
            let connect_fut = async {
                let tcp_stream = if let Some(local_addr) = local_address {
                    R::connect_bound(addr, local_addr)
                        .await
                        .map_err(Error::Io)?
                } else {
                    #[cfg(feature = "tower")]
                    if let Some(ref connector) = self.connector {
                        let info = crate::connector::ConnectInfo {
                            uri: original_uri.clone(),
                            addr,
                        };
                        connector.connect(info).await.map_err(Error::Io)?
                    } else {
                        R::connect(addr).await?
                    }
                    #[cfg(not(feature = "tower"))]
                    R::connect(addr).await?
                };
                #[cfg(target_os = "linux")]
                if let Some(iface) = interface {
                    R::bind_device(&tcp_stream, iface)?;
                }
                if let Some(time) = tcp_keepalive {
                    R::set_tcp_keepalive(
                        &tcp_stream,
                        time,
                        tcp_keepalive_interval,
                        tcp_keepalive_retries,
                    )?;
                }
                if is_https {
                    self.connect_tls(tcp_stream, authority.host()).await
                } else {
                    self.connect_plaintext(tcp_stream).await
                }
            };

            let mut conn = match self.connect_timeout {
                Some(duration) => {
                    crate::timeout::Timeout::WithTimeout {
                        future: connect_fut,
                        sleep: R::sleep(duration),
                    }
                    .await?
                }
                None => connect_fut.await?,
            };
            conn.remote_addr = Some(addr);
            conn
        };

        let mut resp = Self::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
        resp.set_remote_addr(pooled.remote_addr);
        resp.set_tls_info(pooled.tls_info.clone());
        if !self.no_connection_reuse && resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            self.pool.checkin(pool_key, pooled);
        }

        Ok(resp)
    }

    async fn connect_via_proxy(
        &self,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
        is_https: bool,
    ) -> Result<PooledConnection<R>> {
        let proxy_authority = proxy.authority()?;
        let default_port = proxy.default_port();
        let proxy_addr = self
            .resolve_authority(proxy_authority, default_port)
            .await?;
        let mut tcp_stream = if let Some(local_addr) = self.local_address {
            R::connect_bound(proxy_addr, local_addr)
                .await
                .map_err(Error::Io)?
        } else {
            R::connect(proxy_addr).await?
        };
        #[cfg(target_os = "linux")]
        if let Some(ref iface) = self.interface {
            R::bind_device(&tcp_stream, iface)?;
        }
        if let Some(time) = self.tcp_keepalive {
            R::set_tcp_keepalive(
                &tcp_stream,
                time,
                self.tcp_keepalive_interval,
                self.tcp_keepalive_retries,
            )?;
        }

        if proxy.scheme == crate::proxy::ProxyScheme::Socks5 {
            let host = target_authority.host();
            let port = target_authority
                .port_u16()
                .unwrap_or(if is_https { 443 } else { 80 });
            crate::socks5::socks5_handshake(&mut tcp_stream, host, port, proxy.auth.as_ref())
                .await
                .map_err(Error::Io)?;
            if is_https {
                self.connect_tls(tcp_stream, host).await
            } else {
                self.connect_h1(tcp_stream).await
            }
        } else if proxy.scheme == crate::proxy::ProxyScheme::Socks4 {
            let host = target_authority.host();
            let port = target_authority
                .port_u16()
                .unwrap_or(if is_https { 443 } else { 80 });
            crate::socks4::socks4a_handshake(&mut tcp_stream, host, port, proxy.auth.as_ref())
                .await
                .map_err(Error::Io)?;
            if is_https {
                self.connect_tls(tcp_stream, host).await
            } else {
                self.connect_h1(tcp_stream).await
            }
        } else if is_https {
            self.connect_tunnel(tcp_stream, proxy, target_authority)
                .await
        } else {
            self.connect_plaintext(tcp_stream).await
        }
    }

    async fn connect_tunnel(
        &self,
        mut tcp_stream: R::TcpStream,
        proxy: &ProxyConfig,
        target_authority: &http::uri::Authority,
    ) -> Result<PooledConnection<R>> {
        use hyper::rt::{Read, Write};

        let target = target_authority.as_str();

        let mut connect_msg = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(auth_value) = proxy.connect_header(target) {
            connect_msg.push_str(&format!("Proxy-Authorization: {auth_value}\r\n"));
        }
        connect_msg.push_str("\r\n");

        let buf = connect_msg.into_bytes();
        let mut written = 0;
        while written < buf.len() {
            let n = std::future::poll_fn(|cx| {
                Pin::new(&mut tcp_stream).poll_write(cx, &buf[written..])
            })
            .await
            .map_err(Error::Io)?;
            written += n;
        }

        let mut resp_buf = Vec::with_capacity(256);
        loop {
            let mut one = [0u8; 1];
            let mut read_buf = hyper::rt::ReadBuf::new(&mut one);
            std::future::poll_fn(|cx| Pin::new(&mut tcp_stream).poll_read(cx, read_buf.unfilled()))
                .await
                .map_err(Error::Io)?;

            if read_buf.filled().is_empty() {
                return Err(Error::Other("proxy closed connection".into()));
            }
            resp_buf.push(one[0]);
            read_buf = hyper::rt::ReadBuf::new(&mut one);

            if resp_buf.len() >= 4 && resp_buf[resp_buf.len() - 4..] == *b"\r\n\r\n" {
                break;
            }

            if resp_buf.len() > 8192 {
                return Err(Error::Other("CONNECT response too large".into()));
            }
        }

        let resp_str = String::from_utf8_lossy(&resp_buf);
        let status_line = resp_str
            .lines()
            .next()
            .ok_or_else(|| Error::Other("empty CONNECT response".into()))?;

        if !status_line.contains("200") {
            return Err(Error::Other(
                format!("CONNECT tunnel failed: {status_line}").into(),
            ));
        }

        self.connect_tls(tcp_stream, target_authority.host()).await
    }

    fn connect_plaintext<S>(
        &self,
        stream: S,
    ) -> Pin<Box<dyn Future<Output = Result<PooledConnection<R>>> + Send + '_>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        if self.http2_prior_knowledge {
            Box::pin(self.connect_h2_prior_knowledge(stream))
        } else {
            Box::pin(self.connect_h1(stream))
        }
    }

    async fn connect_h1<S>(&self, stream: S) -> Result<PooledConnection<R>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let (sender, conn) = hyper::client::conn::http1::handshake(stream).await?;
        R::spawn(async move {
            let _ = conn.with_upgrades().await;
        });
        Ok(PooledConnection::new_h1(sender))
    }

    async fn connect_h2_prior_knowledge<S>(
        &self,
        stream: S,
    ) -> Result<PooledConnection<R>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Send + Unpin + 'static,
    {
        let mut builder =
            hyper::client::conn::http2::Builder::new(crate::runtime::hyper_executor::<R>());
        if let Some(ref h2) = self.http2 {
            if let Some(v) = h2.initial_stream_window_size {
                builder.initial_stream_window_size(v);
            }
            if let Some(v) = h2.initial_connection_window_size {
                builder.initial_connection_window_size(v);
            }
            if let Some(v) = h2.max_frame_size {
                builder.max_frame_size(v);
            }
            if let Some(v) = h2.adaptive_window {
                builder.adaptive_window(v);
            }
            if let Some(v) = h2.keep_alive_interval {
                builder.keep_alive_interval(v);
            }
            if let Some(v) = h2.keep_alive_timeout {
                builder.keep_alive_timeout(v);
            }
            if let Some(v) = h2.keep_alive_while_idle {
                builder.keep_alive_while_idle(v);
            }
            if let Some(v) = h2.max_header_list_size {
                builder.max_header_list_size(v);
            }
            if let Some(v) = h2.max_send_buf_size {
                builder.max_send_buf_size(v);
            }
            if let Some(v) = h2.max_concurrent_reset_streams {
                builder.max_concurrent_reset_streams(v);
            }
        }
        let (sender, conn) = builder.handshake(stream).await?;
        R::spawn(async move {
            let _ = conn.await;
        });
        Ok(PooledConnection::new_h2(sender))
    }

    #[cfg(feature = "rustls")]
    async fn connect_tls(
        &self,
        tcp_stream: R::TcpStream,
        host: &str,
    ) -> Result<PooledConnection<R>> {
        use crate::tls::TlsConnect;

        let tls_connector = self
            .tls
            .as_ref()
            .ok_or_else(|| Error::Tls("no TLS connector configured".into()))?;

        let tls_stream = <crate::tls::RustlsConnector as TlsConnect<R>>::connect(
            tls_connector,
            host,
            tcp_stream,
        )
        .await
        .map_err(|e| Error::Tls(Box::new(e)))?;

        let alpn = crate::tls::RustlsConnector::negotiated_protocol(tls_stream.tls_connection());
        let tls_info = tls_stream.tls_info();

        match alpn {
            Some(crate::tls::AlpnProtocol::H2) => {
                let mut builder =
                    hyper::client::conn::http2::Builder::new(crate::runtime::hyper_executor::<R>());
                if let Some(ref h2) = self.http2 {
                    if let Some(v) = h2.initial_stream_window_size {
                        builder.initial_stream_window_size(v);
                    }
                    if let Some(v) = h2.initial_connection_window_size {
                        builder.initial_connection_window_size(v);
                    }
                    if let Some(v) = h2.max_frame_size {
                        builder.max_frame_size(v);
                    }
                    if let Some(v) = h2.adaptive_window {
                        builder.adaptive_window(v);
                    }
                    if let Some(v) = h2.keep_alive_interval {
                        builder.keep_alive_interval(v);
                    }
                    if let Some(v) = h2.keep_alive_timeout {
                        builder.keep_alive_timeout(v);
                    }
                    if let Some(v) = h2.keep_alive_while_idle {
                        builder.keep_alive_while_idle(v);
                    }
                    if let Some(v) = h2.max_header_list_size {
                        builder.max_header_list_size(v);
                    }
                    if let Some(v) = h2.max_send_buf_size {
                        builder.max_send_buf_size(v);
                    }
                    if let Some(v) = h2.max_concurrent_reset_streams {
                        builder.max_concurrent_reset_streams(v);
                    }
                }
                let (sender, conn) = builder.handshake(tls_stream).await?;
                R::spawn(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h2(sender);
                pooled.tls_info = Some(tls_info);
                Ok(pooled)
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn(async move {
                    let _ = conn.await;
                });
                let mut pooled = PooledConnection::new_h1(sender);
                pooled.tls_info = Some(tls_info);
                Ok(pooled)
            }
        }
    }

    #[cfg(not(feature = "rustls"))]
    async fn connect_tls(
        &self,
        _tcp_stream: R::TcpStream,
        _host: &str,
    ) -> Result<PooledConnection<R>> {
        Err(Error::Tls("HTTPS requires the `rustls` feature".into()))
    }

    async fn send_on_connection(
        conn: &mut PooledConnection<R>,
        request: http::Request<HyperBody>,
        url: Uri,
    ) -> Result<Response> {
        match &mut conn.conn {
            HttpConnection::H1(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(|body| body.map_err(Error::Hyper).boxed());
                Ok(Response::new(resp, url))
            }
            HttpConnection::H2(sender) => {
                let resp = sender.send_request(request).await?;
                let resp = resp.map(|body| body.map_err(Error::Hyper).boxed());
                Ok(Response::new(resp, url))
            }
            #[cfg(feature = "http3")]
            HttpConnection::H3(sender) => {
                crate::h3_transport::send_on_h3(sender, request, url).await
            }
        }
    }

    async fn resolve_authority(
        &self,
        authority: &http::uri::Authority,
        default_port: u16,
    ) -> Result<std::net::SocketAddr> {
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(default_port);
        self.resolve_authority_raw(host, port).await
    }

    async fn resolve_authority_raw(&self, host: &str, port: u16) -> Result<std::net::SocketAddr> {
        if let Ok(addr) = format!("{host}:{port}").parse() {
            return Ok(addr);
        }

        if let Some(resolver) = &self.resolver {
            return resolver
                .resolve(host, port)
                .await
                .map_err(|e| Error::InvalidUrl(format!("cannot resolve {host}:{port}: {e}")));
        }

        R::resolve(host, port)
            .await
            .map_err(|e| Error::InvalidUrl(format!("cannot resolve {host}:{port}: {e}")))
    }

    #[cfg(feature = "http3")]
    fn cache_alt_svc(&self, uri: &Uri, headers: &HeaderMap) {
        use http::header::ALT_SVC;
        if let Some(authority) = uri.authority() {
            if let Some(alt_svc_value) = headers.get(ALT_SVC) {
                if let Ok(value_str) = alt_svc_value.to_str() {
                    let entries = crate::alt_svc::parse_alt_svc(value_str);
                    self.alt_svc_cache.insert(authority.clone(), entries);
                }
            }
        }
    }
}

fn resolve_redirect(base: &Uri, location: &str) -> Result<Uri> {
    if let Ok(absolute) = location.parse::<Uri>() {
        if absolute.scheme().is_some() {
            return Ok(absolute);
        }
    }

    let scheme = base
        .scheme_str()
        .ok_or_else(|| Error::InvalidUrl("missing scheme in base".into()))?;
    let authority = base
        .authority()
        .ok_or_else(|| Error::InvalidUrl("missing authority in base".into()))?;

    let new_uri = format!("{scheme}://{authority}{location}");
    new_uri
        .parse()
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))
}
