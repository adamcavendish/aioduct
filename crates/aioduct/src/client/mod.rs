mod builder;
mod connect;
mod dispatch;
mod resolve;

pub use builder::ClientBuilder;

use std::marker::PhantomData;
use std::net::IpAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{
    AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderMap,
    HeaderValue, LOCATION, PROXY_AUTHORIZATION, REFERER,
};
use http::{Method, StatusCode, Uri};
use http_body_util::BodyExt;

use crate::body::RequestBody;
use crate::cache::HttpCache;
use crate::cookie::CookieJar;
use crate::error::{AioductBody, Error};
use crate::http2::Http2Config;
use crate::middleware::MiddlewareStack;
use crate::pool::ConnectionPool;
use crate::proxy::ProxySettings;
use crate::redirect::{RedirectAction, RedirectPolicy};
use crate::request::RequestBuilder;
use crate::response::Response;
use crate::retry::RetryConfig;
use crate::runtime::{Resolve, Runtime};

const DEFAULT_USER_AGENT: &str = concat!("aioduct/", env!("CARGO_PKG_VERSION"));

/// HTTP client with connection pooling, TLS, and automatic redirect handling.
pub struct Client<R: Runtime> {
    pub(crate) pool: ConnectionPool<R>,
    pub(crate) redirect_policy: RedirectPolicy,
    pub(crate) timeout: Option<Duration>,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) read_timeout: Option<Duration>,
    pub(crate) tcp_keepalive: Option<Duration>,
    pub(crate) tcp_keepalive_interval: Option<Duration>,
    pub(crate) tcp_keepalive_retries: Option<u32>,
    pub(crate) local_address: Option<IpAddr>,
    #[cfg(target_os = "linux")]
    pub(crate) interface: Option<String>,
    #[cfg(unix)]
    pub(crate) unix_socket: Option<PathBuf>,
    pub(crate) https_only: bool,
    pub(crate) referer: bool,
    pub(crate) no_connection_reuse: bool,
    pub(crate) tcp_fast_open: bool,
    pub(crate) http2_prior_knowledge: bool,
    pub(crate) accept_encoding: crate::decompress::AcceptEncoding,
    pub(crate) default_headers: HeaderMap,
    pub(crate) retry: Option<RetryConfig>,
    pub(crate) cookie_jar: Option<CookieJar>,
    pub(crate) proxy: Option<ProxySettings>,
    pub(crate) resolver: Option<Arc<dyn Resolve>>,
    pub(crate) http2: Option<Http2Config>,
    pub(crate) middleware: MiddlewareStack,
    pub(crate) rate_limiter: Option<crate::throttle::RateLimiter>,
    pub(crate) bandwidth_limiter: Option<crate::bandwidth::BandwidthLimiter>,
    pub(crate) digest_auth: Option<crate::digest_auth::DigestAuth>,
    pub(crate) cache: Option<HttpCache>,
    pub(crate) hsts: Option<crate::hsts::HstsStore>,
    #[cfg(feature = "tower")]
    pub(crate) connector: Option<crate::connector::LayeredConnector<R>>,
    #[cfg(feature = "rustls")]
    pub(crate) tls: Option<Arc<crate::tls::RustlsConnector>>,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) h3_endpoint: Option<quinn::Endpoint>,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) prefer_h3: bool,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) alt_svc_cache: crate::alt_svc::AltSvcCache,
    pub(crate) _runtime: PhantomData<R>,
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
            tcp_fast_open: self.tcp_fast_open,
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
            bandwidth_limiter: self.bandwidth_limiter.clone(),
            digest_auth: self.digest_auth.clone(),
            cache: self.cache.clone(),
            hsts: self.hsts.clone(),
            #[cfg(feature = "tower")]
            connector: self.connector.clone(),
            #[cfg(feature = "rustls")]
            tls: self.tls.clone(),
            #[cfg(all(feature = "http3", feature = "rustls"))]
            h3_endpoint: self.h3_endpoint.clone(),
            #[cfg(all(feature = "http3", feature = "rustls"))]
            prefer_h3: self.prefer_h3,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            alt_svc_cache: self.alt_svc_cache.clone(),
            _runtime: PhantomData,
        }
    }
}

impl<R: Runtime> std::fmt::Debug for Client<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish()
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

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Create a client configured for HTTP/3 with rustls.
    pub fn with_http3() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .http3(true)
            .build()
    }

    #[cfg(all(feature = "http3", feature = "rustls"))]
    /// Create a client that upgrades to HTTP/3 via Alt-Svc discovery.
    pub fn with_alt_svc_h3() -> Self {
        Self::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .alt_svc_h3(true)
            .build()
    }

    /// Start a GET request to the given URL.
    pub fn get(&self, uri: &str) -> Result<RequestBuilder<'_, R>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::GET, uri))
    }

    /// Start a HEAD request to the given URL.
    pub fn head(&self, uri: &str) -> Result<RequestBuilder<'_, R>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::HEAD, uri))
    }

    /// Start a POST request to the given URL.
    pub fn post(&self, uri: &str) -> Result<RequestBuilder<'_, R>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::POST, uri))
    }

    /// Start a PUT request to the given URL.
    pub fn put(&self, uri: &str) -> Result<RequestBuilder<'_, R>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::PUT, uri))
    }

    /// Start a PATCH request to the given URL.
    pub fn patch(&self, uri: &str) -> Result<RequestBuilder<'_, R>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::PATCH, uri))
    }

    /// Start a DELETE request to the given URL.
    pub fn delete(&self, uri: &str) -> Result<RequestBuilder<'_, R>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, Method::DELETE, uri))
    }

    /// Start a request with the given method and URL.
    pub fn request(&self, method: Method, uri: &str) -> Result<RequestBuilder<'_, R>, Error> {
        let uri: Uri = uri.parse().map_err(|e| Error::InvalidUrl(format!("{e}")))?;
        Ok(RequestBuilder::new(self, method, uri))
    }

    /// Start a parallel chunk download for the given URL.
    pub fn chunk_download(&self, url: &str) -> crate::chunk_download::ChunkDownload<R> {
        crate::chunk_download::ChunkDownload::new(self.clone(), url.to_owned())
    }

    /// Forward an incoming HTTP request to an upstream server.
    ///
    /// This is the entry point for proxy/gateway use cases. The forwarded request
    /// bypasses all client middleware (redirects, cookies, cache, decompression)
    /// and streams the body directly to the upstream.
    pub fn forward<B>(&self, request: http::Request<B>) -> crate::forward::ForwardBuilder<'_, R, B>
    where
        B: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        crate::forward::ForwardBuilder::new(self, request)
    }

    pub(crate) fn default_timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub(crate) fn default_retry(&self) -> Option<&RetryConfig> {
        self.retry.as_ref()
    }

    pub(crate) fn middleware(&self) -> &crate::middleware::MiddlewareStack {
        &self.middleware
    }

    /// Returns the bandwidth limiter if one was configured via [`ClientBuilder::max_download_speed`].
    pub fn bandwidth_limiter(&self) -> Option<&crate::bandwidth::BandwidthLimiter> {
        self.bandwidth_limiter.as_ref()
    }

    pub(crate) async fn execute(
        &self,
        method: Method,
        original_uri: Uri,
        headers: http::HeaderMap,
        body: Option<RequestBody>,
        version: Option<http::Version>,
    ) -> Result<Response, Error> {
        if self.https_only && original_uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
            return Err(Error::HttpsOnly(
                original_uri.scheme_str().unwrap_or("none").to_owned(),
            ));
        }

        let mut current_uri = original_uri;

        // HSTS: upgrade http:// to https:// for known HSTS hosts
        if let Some(ref hsts) = self.hsts
            && current_uri.scheme() == Some(&http::uri::Scheme::HTTP)
            && let Some(authority) = current_uri.authority()
            && hsts.should_upgrade(authority.host())
        {
            let upgraded = format!(
                "https://{}{}",
                authority,
                current_uri
                    .path_and_query()
                    .map(|pq| pq.as_str())
                    .unwrap_or("/")
            );
            if let Ok(uri) = upgraded.parse() {
                current_uri = uri;
            }
        }

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
            if let Some(jar) = &self.cookie_jar
                && let Some(authority) = current_uri.authority()
            {
                let is_secure = current_uri.scheme() == Some(&http::uri::Scheme::HTTPS);
                let path = current_uri.path();
                jar.apply_to_request(authority.host(), is_secure, path, &mut current_headers);
            }

            let (req_body, body_for_replay) = match current_body.take() {
                Some(RequestBody::Buffered(b)) => {
                    let body_clone = RequestBody::Buffered(b.clone());
                    (RequestBody::Buffered(b).into_hyper_body(), Some(body_clone))
                }
                Some(rb @ RequestBody::Streaming(_)) => (rb.into_hyper_body(), None),
                None => {
                    let empty: AioductBody = http_body_util::Full::new(Bytes::new())
                        .map_err(|never| match never {})
                        .boxed();
                    (empty, None)
                }
            };

            if !current_headers.contains_key(HOST)
                && let Some(authority) = current_uri.authority()
                && let Ok(host_value) = authority.as_str().parse()
            {
                current_headers.insert(HOST, host_value);
            }

            // Cache lookup: return fresh cached response or prepare conditional headers
            let (cache_state, stale_if_error) = if let Some(ref cache) = self.cache {
                match cache.lookup(&current_method, &current_uri) {
                    crate::cache::CacheLookup::Fresh(cached) => {
                        let http_resp = cached.into_http_response();
                        return Ok(Response::from_boxed(http_resp, current_uri));
                    }
                    crate::cache::CacheLookup::Stale {
                        validators,
                        cached,
                        stale_if_error,
                    } => {
                        validators.apply_to_request(&mut current_headers);
                        (Some(cached), stale_if_error)
                    }
                    crate::cache::CacheLookup::Miss => (None, None),
                }
            } else {
                (None, None)
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

            let resp = match self.execute_single(request, &current_uri).await {
                Ok(resp) => {
                    if resp.status().is_server_error()
                        && let Some(sie_duration) = stale_if_error
                        && let Some(ref cached) = cache_state
                        && cached.age <= sie_duration
                    {
                        let _ = resp.bytes().await;
                        let http_resp = cache_state.unwrap().into_http_response();
                        return Ok(Response::from_boxed(http_resp, current_uri));
                    }
                    resp
                }
                Err(e) => {
                    if let Some(sie_duration) = stale_if_error
                        && let Some(cached) = cache_state
                        && cached.age <= sie_duration
                    {
                        let http_resp = cached.into_http_response();
                        return Ok(Response::from_boxed(http_resp, current_uri));
                    }
                    return Err(e);
                }
            };

            // Digest auth: retry once with credentials on 401 + WWW-Authenticate: Digest
            let resp = if let Some(ref digest) = self.digest_auth {
                if digest.needs_retry(resp.status(), resp.headers()) {
                    if let Some(auth_value) =
                        digest.authorize(&current_method, &current_uri, resp.headers())
                    {
                        let _ = resp.bytes().await;
                        current_headers.insert(AUTHORIZATION, auth_value);

                        let retry_body =
                            match body_for_replay.as_ref().and_then(RequestBody::try_clone) {
                                Some(rb) => rb.into_hyper_body(),
                                None => http_body_util::Full::new(Bytes::new())
                                    .map_err(|never| match never {})
                                    .boxed(),
                            };

                        let retry_uri: Uri = current_uri
                            .path_and_query()
                            .map(|pq| pq.as_str())
                            .unwrap_or("/")
                            .parse()
                            .map_err(|e| Error::Other(Box::new(e)))?;
                        let mut retry_builder = http::Request::builder()
                            .method(current_method.clone())
                            .uri(retry_uri);
                        if let Some(ver) = version {
                            retry_builder = retry_builder.version(ver);
                        }
                        for (name, value) in &current_headers {
                            retry_builder = retry_builder.header(name, value);
                        }
                        let mut retry_request = retry_builder.body(retry_body)?;
                        if !self.middleware.is_empty() {
                            self.middleware
                                .apply_request(&mut retry_request, &current_uri);
                        }
                        self.execute_single(retry_request, &current_uri).await?
                    } else {
                        resp
                    }
                } else {
                    resp
                }
            } else {
                resp
            };

            // Handle 304 Not Modified: reuse cached response
            if resp.status() == StatusCode::NOT_MODIFIED
                && let Some(cached) = cache_state
            {
                let http_resp = cached.into_http_response();
                return Ok(Response::from_boxed(http_resp, current_uri));
            }

            // Invalidate cache on unsafe methods
            if let Some(ref cache) = self.cache {
                cache.invalidate(&current_method, &current_uri);
            }

            if let Some(jar) = &self.cookie_jar
                && let Some(authority) = current_uri.authority()
            {
                jar.store_from_response(authority.host(), resp.headers());
            }

            // HSTS: store STS header from HTTPS responses
            if let Some(ref hsts) = self.hsts
                && current_uri.scheme() == Some(&http::uri::Scheme::HTTPS)
                && let Some(authority) = current_uri.authority()
            {
                hsts.store_from_response(authority.host(), resp.headers());
            }

            if !resp.status().is_redirection()
                || resp.status() == StatusCode::NOT_MODIFIED
                || matches!(self.redirect_policy, RedirectPolicy::None)
            {
                #[cfg(all(feature = "http3", feature = "rustls"))]
                if self.h3_endpoint.is_some() {
                    self.cache_alt_svc(&current_uri, resp.headers());
                }
                let mut resp = resp;
                if !self.middleware.is_empty() {
                    resp.apply_middleware(&self.middleware, &current_uri);
                }

                let resp = if !self.accept_encoding.is_empty() {
                    resp.decompress(&self.accept_encoding)
                } else {
                    resp
                };
                let resp = if let Some(read_timeout) = self.read_timeout {
                    resp.apply_read_timeout::<R>(read_timeout)
                } else {
                    resp
                };

                // Store cacheable responses in the HTTP cache after normal response
                // finalization so cache hits match what callers see.
                if let Some(ref cache) = self.cache {
                    let status = resp.status();
                    let headers = resp.headers().clone();
                    if crate::cache::is_response_cacheable(status, &headers) {
                        let body_bytes = resp.bytes().await?;
                        cache.store(&current_method, &current_uri, status, &headers, &body_bytes);
                        let cached_resp = boxed_response_from_bytes(status, &headers, body_bytes);
                        return Ok(Response::from_boxed(cached_resp, current_uri));
                    }
                }

                return Ok(resp);
            }

            let status = resp.status();
            let location = resp
                .headers()
                .get(LOCATION)
                .ok_or_else(|| Error::Redirect("missing Location header".into()))?
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
                return Ok(Response::from_boxed(
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

            if !self.middleware.is_empty() {
                self.middleware
                    .apply_redirect(status, &current_uri, &next_uri);
            }

            match status {
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER => {
                    current_method = Method::GET;
                    current_body = None;
                    current_headers.remove(CONTENT_TYPE);
                    current_headers.remove(CONTENT_LENGTH);
                    current_headers.remove(CONTENT_ENCODING);
                }
                StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => {
                    current_body = body_for_replay;
                }
                _ => return Err(Error::Redirect("unexpected redirect status".into())),
            }

            // Update Host header for the new target
            if let Some(authority) = next_uri.authority()
                && let Ok(host_value) = authority.as_str().parse()
            {
                current_headers.insert(HOST, host_value);
            }

            // Strip sensitive headers on cross-origin redirect
            let same_origin = current_uri.authority() == next_uri.authority()
                && current_uri.scheme() == next_uri.scheme();
            if !same_origin {
                current_headers.remove(AUTHORIZATION);
                current_headers.remove(COOKIE);
                current_headers.remove(PROXY_AUTHORIZATION);
            }

            if self.referer
                && let Ok(val) = HeaderValue::from_str(&current_uri.to_string())
            {
                current_headers.insert(REFERER, val);
            }

            current_uri = next_uri;
        }

        Err(Error::TooManyRedirects(
            self.redirect_policy.max_redirects(),
        ))
    }
}

fn resolve_redirect(base: &Uri, location: &str) -> Result<Uri, Error> {
    base.scheme_str()
        .ok_or_else(|| Error::InvalidUrl("missing scheme in base".into()))?;
    base.authority()
        .ok_or_else(|| Error::InvalidUrl("missing authority in base".into()))?;

    let base_url =
        url::Url::parse(&base.to_string()).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    let mut next = base_url
        .join(location)
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))?;
    next.set_fragment(None);
    next.as_str()
        .parse()
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))
}

fn boxed_response_from_bytes(
    status: StatusCode,
    headers: &HeaderMap,
    body: Bytes,
) -> http::Response<AioductBody> {
    let mut builder = http::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(
            http_body_util::Full::new(body)
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("response builder with valid status cannot fail")
}

#[cfg(test)]
mod tests;
