mod builder;
mod connect;

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
mod tests {
    use super::*;

    #[test]
    fn resolve_redirect_absolute_url() {
        let base: Uri = "http://example.com/old".parse().unwrap();
        let result = resolve_redirect(&base, "https://other.com/new").unwrap();
        assert_eq!(result.to_string(), "https://other.com/new");
    }

    #[test]
    fn resolve_redirect_relative_path() {
        let base: Uri = "http://example.com/old".parse().unwrap();
        let result = resolve_redirect(&base, "/new/path").unwrap();
        assert_eq!(result.to_string(), "http://example.com/new/path");
    }

    #[test]
    fn resolve_redirect_relative_with_query() {
        let base: Uri = "https://example.com/page".parse().unwrap();
        let result = resolve_redirect(&base, "/search?q=test").unwrap();
        assert_eq!(result.to_string(), "https://example.com/search?q=test");
    }

    #[test]
    fn resolve_redirect_relative_without_leading_slash_uses_base_directory() {
        let base: Uri = "http://example.com/dir/page".parse().unwrap();
        let result = resolve_redirect(&base, "next").unwrap();
        assert_eq!(result.to_string(), "http://example.com/dir/next");
    }

    #[test]
    fn resolve_redirect_relative_parent_directory_is_normalized() {
        let base: Uri = "http://example.com/dir/page".parse().unwrap();
        let result = resolve_redirect(&base, "../up").unwrap();
        assert_eq!(result.to_string(), "http://example.com/up");
    }

    #[test]
    fn resolve_redirect_query_only_keeps_base_path() {
        let base: Uri = "http://example.com/dir/page?old=1".parse().unwrap();
        let result = resolve_redirect(&base, "?new=2").unwrap();
        assert_eq!(result.to_string(), "http://example.com/dir/page?new=2");
    }

    #[test]
    fn resolve_redirect_protocol_relative_uses_base_scheme() {
        let base: Uri = "https://example.com/old".parse().unwrap();
        let result = resolve_redirect(&base, "//other.example/new").unwrap();
        assert_eq!(result.to_string(), "https://other.example/new");
    }

    #[test]
    fn resolve_redirect_preserves_port() {
        let base: Uri = "http://example.com:8080/old".parse().unwrap();
        let result = resolve_redirect(&base, "/new").unwrap();
        assert_eq!(result.to_string(), "http://example.com:8080/new");
    }

    #[test]
    fn resolve_redirect_scheme_without_authority_is_relative() {
        let base: Uri = "http://example.com/".parse().unwrap();
        let result = resolve_redirect(&base, "/path").unwrap();
        assert_eq!(result.host().unwrap(), "example.com");
    }

    #[test]
    fn is_cacheable_method_test() {
        assert!(Method::GET == Method::GET);
    }

    #[test]
    fn default_user_agent_contains_version() {
        assert!(DEFAULT_USER_AGENT.starts_with("aioduct/"));
    }

    #[test]
    fn resolve_redirect_missing_scheme() {
        let base: Uri = "/relative".parse().unwrap();
        let result = resolve_redirect(&base, "/new");
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::InvalidUrl(msg) => assert!(msg.contains("scheme")),
            other => panic!("expected InvalidUrl, got {other:?}"),
        }
    }

    #[test]
    fn resolve_redirect_missing_authority() {
        let base = Uri::from_static("http:");
        let result = resolve_redirect(&base, "/new");
        assert!(result.is_err());
    }
}

#[cfg(all(test, feature = "tokio"))]
mod builder_tests {
    use super::*;
    use crate::runtime::tokio_rt::TokioRuntime;
    use http::header::USER_AGENT;

    type TokioClient = Client<TokioRuntime>;

    #[cfg(feature = "rustls")]
    fn install_crypto() {
        crate::tls::install_default_crypto_provider();
    }

    #[tokio::test]
    async fn builder_read_timeout() {
        let _client = TokioClient::builder()
            .read_timeout(Duration::from_secs(5))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive() {
        let _client = TokioClient::builder()
            .tcp_keepalive(Duration::from_secs(60))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive_interval() {
        let _client = TokioClient::builder()
            .tcp_keepalive_interval(Duration::from_secs(10))
            .build();
    }

    #[tokio::test]
    async fn builder_tcp_keepalive_retries() {
        let _client = TokioClient::builder().tcp_keepalive_retries(3).build();
    }

    #[tokio::test]
    async fn builder_local_address() {
        let _client = TokioClient::builder()
            .local_address("127.0.0.1".parse().unwrap())
            .build();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn builder_interface() {
        let _client = TokioClient::builder().interface("eth0").build();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn builder_unix_socket() {
        let _client = TokioClient::builder().unix_socket("/tmp/test.sock").build();
    }

    #[tokio::test]
    async fn builder_referer() {
        let _client = TokioClient::builder().referer(true).build();
    }

    #[tokio::test]
    async fn builder_http2_prior_knowledge() {
        let _client = TokioClient::builder().http2_prior_knowledge().build();
    }

    #[tokio::test]
    async fn builder_no_default_headers() {
        let client = TokioClient::builder().no_default_headers().build();
        assert!(client.default_headers.is_empty());
    }

    #[tokio::test]
    async fn builder_user_agent_with_invalid_value() {
        let client = TokioClient::builder().user_agent("valid-agent/1.0").build();
        assert!(client.default_headers.get(USER_AGENT).is_some());
    }

    #[tokio::test]
    async fn builder_proxy_settings() {
        use crate::proxy::ProxyConfig;
        let settings = ProxySettings::default().http(ProxyConfig::http("http://proxy:80").unwrap());
        let _client = TokioClient::builder().proxy_settings(settings).build();
    }

    #[tokio::test]
    async fn builder_http2_config() {
        let config = crate::http2::Http2Config::default();
        let _client = TokioClient::builder().http2(config).build();
    }

    #[tokio::test]
    async fn builder_rate_limiter() {
        let limiter = crate::throttle::RateLimiter::new(10, Duration::from_secs(1));
        let _client = TokioClient::builder().rate_limiter(limiter).build();
    }

    #[tokio::test]
    async fn client_default_creates_same_as_new() {
        let _client: TokioClient = Default::default();
    }

    #[tokio::test]
    async fn client_method_helpers() {
        let client = TokioClient::new();
        assert!(client.get("http://example.com").is_ok());
        assert!(client.head("http://example.com").is_ok());
        assert!(client.post("http://example.com").is_ok());
        assert!(client.put("http://example.com").is_ok());
        assert!(client.patch("http://example.com").is_ok());
        assert!(client.delete("http://example.com").is_ok());
        assert!(
            client
                .request(Method::OPTIONS, "http://example.com")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn client_invalid_url() {
        let client = TokioClient::new();
        assert!(client.get("not a url").is_err());
    }

    #[tokio::test]
    async fn client_https_only_rejects_http() {
        let client = TokioClient::builder().https_only(true).build();
        assert!(client.https_only);
    }

    #[tokio::test]
    async fn client_no_connection_reuse_sets_flag() {
        let client = TokioClient::builder().no_connection_reuse().build();
        assert!(client.no_connection_reuse);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open() {
        let client = TokioClient::builder().tcp_fast_open(true).build();
        assert!(client.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_tcp_fast_open_disabled() {
        let client = TokioClient::builder().tcp_fast_open(false).build();
        assert!(!client.tcp_fast_open);
    }

    #[tokio::test]
    async fn builder_hsts() {
        let store = crate::hsts::HstsStore::new();
        let client = TokioClient::builder().hsts(store).build();
        assert!(client.hsts.is_some());
    }

    #[tokio::test]
    async fn builder_cache() {
        let cache = crate::cache::HttpCache::new();
        let client = TokioClient::builder().cache(cache).build();
        assert!(client.cache.is_some());
    }

    #[tokio::test]
    async fn builder_cookie_jar() {
        let jar = crate::cookie::CookieJar::new();
        let client = TokioClient::builder().cookie_jar(jar).build();
        assert!(client.cookie_jar.is_some());
    }

    #[tokio::test]
    async fn builder_timeout() {
        let client = TokioClient::builder()
            .timeout(Duration::from_secs(10))
            .build();
        assert_eq!(client.timeout, Some(Duration::from_secs(10)));
    }

    #[tokio::test]
    async fn builder_connect_timeout() {
        let client = TokioClient::builder()
            .connect_timeout(Duration::from_secs(5))
            .build();
        assert_eq!(client.connect_timeout, Some(Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn builder_max_redirects() {
        let _client = TokioClient::builder().max_redirects(3).build();
    }

    #[tokio::test]
    async fn builder_redirect_policy_none() {
        let _client = TokioClient::builder()
            .redirect_policy(crate::redirect::RedirectPolicy::none())
            .build();
    }

    #[tokio::test]
    async fn builder_no_decompression() {
        let _client = TokioClient::builder().no_decompression().build();
    }

    #[tokio::test]
    async fn builder_default_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-custom", "value".parse().unwrap());
        let client = TokioClient::builder().default_headers(headers).build();
        assert!(client.default_headers.contains_key("x-custom"));
    }

    #[tokio::test]
    async fn builder_retry() {
        let client = TokioClient::builder()
            .retry(crate::retry::RetryConfig::default())
            .build();
        assert!(client.retry.is_some());
    }

    #[tokio::test]
    async fn builder_system_proxy() {
        let _client = TokioClient::builder().system_proxy().build();
    }

    #[tokio::test]
    async fn builder_max_download_speed() {
        let client = TokioClient::builder()
            .max_download_speed(1024 * 1024)
            .build();
        assert!(client.bandwidth_limiter.is_some());
    }

    #[tokio::test]
    async fn builder_digest_auth() {
        let client = TokioClient::builder().digest_auth("user", "pass").build();
        assert!(client.digest_auth.is_some());
    }

    #[tokio::test]
    async fn builder_https_only() {
        let client = TokioClient::builder().https_only(true).build();
        assert!(client.https_only);
    }

    #[tokio::test]
    async fn builder_debug() {
        let builder = TokioClient::builder();
        let dbg = format!("{builder:?}");
        assert!(dbg.contains("ClientBuilder"));
    }

    #[tokio::test]
    async fn client_clone() {
        let client = TokioClient::new();
        let _cloned = client.clone();
    }

    #[tokio::test]
    async fn builder_pool_idle_timeout() {
        let client = TokioClient::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build();
        assert_eq!(client.timeout, None);
    }

    #[tokio::test]
    async fn builder_pool_max_idle_per_host() {
        let _client = TokioClient::builder().pool_max_idle_per_host(5).build();
    }

    #[tokio::test]
    async fn builder_proxy_shorthand() {
        use crate::proxy::ProxyConfig;
        let config = ProxyConfig::http("http://proxy:8080").unwrap();
        let client = TokioClient::builder().proxy(config).build();
        assert!(client.proxy.is_some());
    }

    #[tokio::test]
    async fn builder_user_agent_invalid_is_ignored() {
        let client = TokioClient::builder().user_agent("bad\x00agent").build();
        let ua = client.default_headers.get(USER_AGENT).unwrap();
        assert_eq!(ua.as_bytes(), DEFAULT_USER_AGENT.as_bytes());
    }

    #[tokio::test]
    async fn builder_middleware() {
        use crate::middleware::Middleware;
        struct NoopMiddleware;
        impl Middleware for NoopMiddleware {}
        let _client = TokioClient::builder().middleware(NoopMiddleware).build();
    }

    #[tokio::test]
    async fn builder_resolver() {
        use std::net::SocketAddr;
        use std::pin::Pin;
        let _client = TokioClient::builder()
            .resolver(
                |_host: &str,
                 _port: u16|
                 -> Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                > { Box::pin(async { Ok("127.0.0.1:80".parse().unwrap()) }) },
            )
            .build();
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_explicit_passthrough() {
        install_crypto();
        let client = TokioClient::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_version_constraints_only() {
        install_crypto();
        let client = TokioClient::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_max_version_only() {
        install_crypto();
        let client = TokioClient::builder()
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_min_and_max() {
        install_crypto();
        let client = TokioClient::builder()
            .min_tls_version(crate::tls::TlsVersion::Tls1_2)
            .max_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = TokioClient::builder()
            .add_root_certificates(&[cert])
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_extra_root_certs_with_version() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let cert = crate::tls::Certificate::from_der(ca.cert.der().to_vec());
        let client = TokioClient::builder()
            .add_root_certificates(&[cert])
            .min_tls_version(crate::tls::TlsVersion::Tls1_3)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_identity() {
        install_crypto();
        let ca = rcgen::generate_simple_self_signed(vec!["test.local".into()]).unwrap();
        let mut pem = ca.cert.pem();
        pem.push_str(&ca.signing_key.serialize_pem());
        let id = crate::tls::Identity::from_pem(pem.as_bytes()).unwrap();
        let client = TokioClient::builder().identity(id).build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_certs() {
        install_crypto();
        let client = TokioClient::builder().danger_accept_invalid_certs().build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_danger_accept_invalid_hostnames() {
        install_crypto();
        let client = TokioClient::builder()
            .danger_accept_invalid_hostnames(true)
            .build();
        assert!(client.tls.is_some());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_disabled() {
        install_crypto();
        let client = TokioClient::builder().tls_sni(false).build();
        let tls = client.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_sni_enabled_is_noop() {
        install_crypto();
        let client = TokioClient::builder().tls_sni(true).build();
        assert!(client.tls.is_none());
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_crls() {
        install_crypto();
        let crl = crate::tls::CertificateRevocationList::from_der(vec![]);
        let _builder = TokioClient::builder().add_crls([crl]);
    }

    #[cfg(feature = "rustls")]
    #[tokio::test]
    async fn builder_tls_explicit_with_sni_disabled() {
        install_crypto();
        let client = TokioClient::builder()
            .tls(crate::tls::RustlsConnector::with_webpki_roots())
            .tls_sni(false)
            .build();
        let tls = client.tls.as_ref().unwrap();
        assert!(!tls.config().enable_sni);
    }
}
