use std::marker::PhantomData;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{
    AUTHORIZATION, COOKIE, HOST, HeaderMap, HeaderValue, LOCATION, PROXY_AUTHORIZATION, USER_AGENT,
};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;

use crate::body::RequestBody;
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

const DEFAULT_USER_AGENT: &str = concat!("aioduct/", env!("CARGO_PKG_VERSION"));

/// HTTP client with connection pooling, TLS, and automatic redirect handling.
pub struct Client<R: Runtime> {
    pool: ConnectionPool<R>,
    redirect_policy: RedirectPolicy,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    tcp_keepalive: Option<Duration>,
    local_address: Option<IpAddr>,
    https_only: bool,
    accept_encoding: crate::decompress::AcceptEncoding,
    default_headers: HeaderMap,
    retry: Option<RetryConfig>,
    cookie_jar: Option<CookieJar>,
    proxy: Option<ProxySettings>,
    resolver: Option<Arc<dyn Resolve>>,
    http2: Option<Http2Config>,
    middleware: MiddlewareStack,
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
            tcp_keepalive: self.tcp_keepalive,
            local_address: self.local_address,
            https_only: self.https_only,
            accept_encoding: self.accept_encoding.clone(),
            default_headers: self.default_headers.clone(),
            retry: self.retry.clone(),
            cookie_jar: self.cookie_jar.clone(),
            proxy: self.proxy.clone(),
            resolver: self.resolver.clone(),
            http2: self.http2.clone(),
            middleware: self.middleware.clone(),
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
    redirect_policy: RedirectPolicy,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    tcp_keepalive: Option<Duration>,
    local_address: Option<IpAddr>,
    https_only: bool,
    accept_encoding: crate::decompress::AcceptEncoding,
    default_headers: HeaderMap,
    retry: Option<RetryConfig>,
    cookie_jar: Option<CookieJar>,
    proxy: Option<ProxySettings>,
    resolver: Option<Arc<dyn Resolve>>,
    http2: Option<Http2Config>,
    middleware: MiddlewareStack,
    #[cfg(feature = "rustls")]
    tls: Option<Arc<crate::tls::RustlsConnector>>,
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
            redirect_policy: RedirectPolicy::default(),
            timeout: None,
            connect_timeout: None,
            tcp_keepalive: None,
            local_address: None,
            https_only: false,
            accept_encoding: crate::decompress::AcceptEncoding::default(),
            default_headers,
            retry: None,
            cookie_jar: None,
            proxy: None,
            resolver: None,
            http2: None,
            middleware: MiddlewareStack::new(),
            #[cfg(feature = "rustls")]
            tls: None,
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

    /// Enable TCP keepalive with the given interval.
    pub fn tcp_keepalive(mut self, interval: Duration) -> Self {
        self.tcp_keepalive = Some(interval);
        self
    }

    /// Bind outgoing connections to a specific local IP address.
    pub fn local_address(mut self, addr: IpAddr) -> Self {
        self.local_address = Some(addr);
        self
    }

    /// Only allow HTTPS URLs; reject plain HTTP requests with an error.
    pub fn https_only(mut self, enable: bool) -> Self {
        self.https_only = enable;
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

    #[cfg(feature = "rustls")]
    /// Set the TLS connector for HTTPS.
    pub fn tls(mut self, connector: crate::tls::RustlsConnector) -> Self {
        self.tls = Some(Arc::new(connector));
        self
    }

    #[cfg(feature = "rustls")]
    /// Accept invalid TLS certificates (INSECURE — for testing/dev only).
    pub fn danger_accept_invalid_certs(self) -> Self {
        self.tls(crate::tls::RustlsConnector::danger_accept_invalid_certs())
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
        Client {
            pool: ConnectionPool::new(self.pool_max_idle_per_host, self.pool_idle_timeout),
            redirect_policy: self.redirect_policy,
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            tcp_keepalive: self.tcp_keepalive,
            local_address: self.local_address,
            https_only: self.https_only,
            accept_encoding: self.accept_encoding,
            default_headers: self.default_headers,
            retry: self.retry,
            cookie_jar: self.cookie_jar,
            proxy: self.proxy,
            resolver: self.resolver,
            http2: self.http2,
            middleware: self.middleware,
            #[cfg(feature = "rustls")]
            tls: self.tls,
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
                return Ok(resp.decompress(&self.accept_encoding));
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
        let scheme = original_uri
            .scheme()
            .ok_or_else(|| Error::InvalidUrl("missing scheme".into()))?;
        let authority = original_uri
            .authority()
            .ok_or_else(|| Error::InvalidUrl("missing authority".into()))?;

        let is_https = scheme == &http::uri::Scheme::HTTPS;

        let pool_key = crate::pool::PoolKey::new(scheme.clone(), authority.clone());

        if let Some(mut conn) = self.pool.checkout(&pool_key) {
            let resp = Self::send_on_connection(&mut conn, request, original_uri.clone()).await?;
            if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                self.pool.checkin(pool_key, conn);
            }
            return Ok(resp);
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
                    let resp = Self::send_on_connection(&mut pooled, request, original_uri.clone())
                        .await?;
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

        let mut pooled = if let Some(proxy) = proxy {
            self.connect_via_proxy(proxy, authority, is_https).await?
        } else {
            let default_port = if is_https { 443 } else { 80 };
            let addr = self.resolve_authority(authority, default_port).await?;

            let tcp_keepalive = self.tcp_keepalive;
            let local_address = self.local_address;
            let connect_fut = async {
                let tcp_stream = if let Some(local_addr) = local_address {
                    R::connect_bound(addr, local_addr)
                        .await
                        .map_err(Error::Io)?
                } else {
                    R::connect(addr).await?
                };
                if let Some(interval) = tcp_keepalive {
                    R::set_tcp_keepalive(&tcp_stream, interval)?;
                }
                if is_https {
                    self.connect_tls(tcp_stream, authority.host()).await
                } else {
                    self.connect_h1(tcp_stream).await
                }
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
        };

        let resp = Self::send_on_connection(&mut pooled, request, original_uri.clone()).await?;
        if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
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
        if let Some(interval) = self.tcp_keepalive {
            R::set_tcp_keepalive(&tcp_stream, interval)?;
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
        } else if is_https {
            self.connect_tunnel(tcp_stream, proxy, target_authority)
                .await
        } else {
            self.connect_h1(tcp_stream).await
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

    async fn connect_h1(&self, tcp_stream: R::TcpStream) -> Result<PooledConnection<R>> {
        let (sender, conn) = hyper::client::conn::http1::handshake(tcp_stream).await?;
        R::spawn(async move {
            let _ = conn.with_upgrades().await;
        });
        Ok(PooledConnection::new_h1(sender))
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
                Ok(PooledConnection::new_h2(sender))
            }
            _ => {
                let (sender, conn) = hyper::client::conn::http1::handshake(tls_stream).await?;
                R::spawn(async move {
                    let _ = conn.await;
                });
                Ok(PooledConnection::new_h1(sender))
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
