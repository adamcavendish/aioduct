mod builder;
mod builder_build_local;
mod builder_build_send;
mod builder_setters;
mod connect_handshake;
mod connect_protocol_local;
mod connect_protocol_send;
mod connection_lifecycle;
mod dispatch_local;
mod dispatch_send;
mod engine_local;
mod engine_send;
mod execute_local;
mod execute_send;
mod proxy_connect_local;
mod proxy_connect_send;
mod replay;
mod request_flow;
mod request_replay_send;
mod resolve;

pub use builder::HttpEngineBuilder;

use std::marker::PhantomData;
use std::net::IpAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
#[cfg(test)]
use http::Method;
use http::header::HeaderMap;
use http::header::HeaderName;
use http::{StatusCode, Uri};
use http_body_util::BodyExt;
use std::collections::HashSet;

pub(crate) use replay::{BodyReplayability, ReplayReason, RequestReplayPolicy};

pub(crate) fn extract_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_owned(),
                v.to_str().unwrap_or("<binary>").to_owned(),
            )
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
use crate::body::RequestBodyLocal;
use crate::body::RequestBodySend;
use crate::cache::HttpCache;
use crate::cookie::CookieJar;
use crate::error::Error;
use crate::h2c_probe::H2cProbeCache;
use crate::http2::Http2Config;
use crate::middleware::MiddlewareStack;
use crate::pool::ConnectionPool;
use crate::proxy::{ProxyChain, ProxySettings};
use crate::redirect::RedirectPolicy;
use crate::retry::RetryConfig;
use crate::runtime::Resolve;

const DEFAULT_USER_AGENT: &str = concat!("aioduct/", env!("CARGO_PKG_VERSION"));

/// Shared configuration for HTTP engines.
///
/// Contains connection pooling, TLS, timeouts, headers, proxy, middleware, and
/// other settings shared between [`HttpEngineSend`] and [`HttpEngineLocal`].
///
/// Generic over `B`, the body type stored in the connection pool:
/// - Send path uses `B = RequestBodySend` (inner body is `Send`)
/// - Local path uses `B = RequestBodyLocal` (inner body may be `!Send`)
pub struct HttpEngineCore<B> {
    pub(crate) pool: ConnectionPool<B>,
    pub(crate) base_url: Option<Arc<url::Url>>,
    pub(crate) address_family: crate::address_family::AddressFamily,
    pub(crate) redirect_policy: RedirectPolicy,
    pub(crate) timeout: Option<Duration>,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) read_timeout: Option<Duration>,
    pub(crate) write_timeout: Option<Duration>,
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
    pub(crate) accept_encoding: crate::decompress::AcceptEncoding,
    pub(crate) accept_encoding_header: Option<http::HeaderValue>,
    pub(crate) default_headers: Arc<HeaderMap>,
    pub(crate) retry: Option<RetryConfig>,
    pub(crate) cookie_jar: Option<CookieJar>,
    pub(crate) proxy: Option<ProxySettings>,
    pub(crate) proxy_chain: Option<ProxyChain>,
    pub(crate) resolver: Option<Arc<dyn Resolve>>,
    pub(crate) http2: Option<Http2Config>,
    pub(crate) middleware: MiddlewareStack,
    pub(crate) rate_limiter: Option<crate::throttle::RateLimiter>,
    pub(crate) bandwidth_limiter: Option<crate::bandwidth::BandwidthLimiter>,
    pub(crate) digest_auth: Option<crate::digest_auth::DigestAuth>,
    pub(crate) message_signature: Option<crate::message_signatures::AutomaticMessageSignature>,
    pub(crate) automatic_content_digest: bool,
    pub(crate) cache: Option<HttpCache>,
    pub(crate) hsts: Option<crate::hsts::HstsStore>,
    pub(crate) h2c_probe_cache: H2cProbeCache,
    pub(crate) connection_coalescing: bool,
    pub(crate) sensitive_headers: HashSet<HeaderName>,
    pub(crate) observer: Option<Arc<dyn crate::observer::RequestObserver>>,
    #[cfg(feature = "rustls")]
    pub(crate) tls: Option<Arc<crate::tls::RustlsConnector>>,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) h3_endpoint: Option<quinn::Endpoint>,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) prefer_h3: bool,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) h3_zero_rtt: bool,
    #[cfg(all(feature = "http3", feature = "rustls"))]
    pub(crate) alt_svc_cache: crate::alt_svc::AltSvcCache,
}

impl<B: 'static> Clone for HttpEngineCore<B> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            base_url: self.base_url.clone(),
            address_family: self.address_family,
            redirect_policy: self.redirect_policy.clone(),
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
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
            accept_encoding: self.accept_encoding.clone(),
            accept_encoding_header: self.accept_encoding_header.clone(),
            default_headers: self.default_headers.clone(),
            retry: self.retry.clone(),
            cookie_jar: self.cookie_jar.clone(),
            proxy: self.proxy.clone(),
            proxy_chain: self.proxy_chain.clone(),
            resolver: self.resolver.clone(),
            http2: self.http2.clone(),
            middleware: self.middleware.clone(),
            rate_limiter: self.rate_limiter.clone(),
            bandwidth_limiter: self.bandwidth_limiter.clone(),
            digest_auth: self.digest_auth.clone(),
            message_signature: self.message_signature.clone(),
            automatic_content_digest: self.automatic_content_digest,
            cache: self.cache.clone(),
            hsts: self.hsts.clone(),
            h2c_probe_cache: self.h2c_probe_cache.clone(),
            connection_coalescing: self.connection_coalescing,
            sensitive_headers: self.sensitive_headers.clone(),
            observer: self.observer.clone(),
            #[cfg(feature = "rustls")]
            tls: self.tls.clone(),
            #[cfg(all(feature = "http3", feature = "rustls"))]
            h3_endpoint: self.h3_endpoint.clone(),
            #[cfg(all(feature = "http3", feature = "rustls"))]
            prefer_h3: self.prefer_h3,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            h3_zero_rtt: self.h3_zero_rtt,
            #[cfg(all(feature = "http3", feature = "rustls"))]
            alt_svc_cache: self.alt_svc_cache.clone(),
        }
    }
}

/// HTTP client for poll-based runtimes (tokio, smol).
///
/// Wraps [`HttpEngineCore`] with a `Send`-capable connector and optional tower layer.
pub struct HttpEngineSend<R, C> {
    pub(crate) core: HttpEngineCore<RequestBodySend>,
    pub(crate) connector: C,
    #[cfg(feature = "tower")]
    pub(crate) tower_connector: Option<crate::connector::TowerConnectorSendSlot>,
    pub(crate) _phantom: PhantomData<R>,
}

impl<R, C: Clone> Clone for HttpEngineSend<R, C> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            connector: self.connector.clone(),
            #[cfg(feature = "tower")]
            tower_connector: self.tower_connector.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<R, C> std::fmt::Debug for HttpEngineSend<R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEngineSend").finish()
    }
}

/// HTTP client for completion-based runtimes (compio).
///
/// Wraps [`HttpEngineCore`] with a `!Send`-capable connector.
pub struct HttpEngineLocal<R, C> {
    pub(crate) core: HttpEngineCore<RequestBodyLocal>,
    pub(crate) connector: C,
    #[cfg(feature = "tower")]
    pub(crate) tower_connector_local: Option<crate::connector::TowerConnectorLocalSlot>,
    pub(crate) _phantom: PhantomData<R>,
}

impl<R, C: Clone> Clone for HttpEngineLocal<R, C> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            connector: self.connector.clone(),
            #[cfg(feature = "tower")]
            tower_connector_local: self.tower_connector_local.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<R, C> std::fmt::Debug for HttpEngineLocal<R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEngineLocal").finish()
    }
}

// ── Pool diagnostics ──────────────────────────────────────────────────────────

impl<B: 'static> HttpEngineCore<B> {
    /// Take a snapshot of connection pool statistics.
    pub fn pool_stats(&self) -> crate::pool::PoolStats {
        self.pool.snapshot()
    }
}

impl<R, C> HttpEngineSend<R, C> {
    /// Take a snapshot of connection pool statistics.
    pub fn pool_stats(&self) -> crate::pool::PoolStats {
        self.core.pool_stats()
    }
}

impl<R, C> HttpEngineLocal<R, C> {
    /// Take a snapshot of connection pool statistics.
    pub fn pool_stats(&self) -> crate::pool::PoolStats {
        self.core.pool_stats()
    }
}

// ── Shared free functions ────────────────────────────────────────────────────

/// Extract the URL fragment from a raw URL string.
///
/// Uses `url::Url` because `http::Uri` strips fragments per RFC 7230.
/// Returns `None` if parsing fails or the URL has no fragment.
pub(crate) fn extract_fragment(raw_url: &str) -> Option<String> {
    url::Url::parse(raw_url)
        .ok()
        .and_then(|u| u.fragment().map(|f| f.to_owned()))
}

/// Resolve a request input URL against an optional client base URL.
///
/// When `base` is `None`, the input is parsed as an absolute URL (existing
/// behavior). When `base` is `Some`, the input is resolved against it per
/// RFC 3986: a relative reference (`users`, `/users`, `?q=1`) resolves against
/// the base, while an absolute URL (with scheme and authority) overrides it.
///
/// When a base is set, the resolved URL is validated: its scheme must be
/// `http` or `https` and it must have a host. This rejects an absolute request
/// override like `get("ftp://host/path")` that would otherwise be dispatched as
/// cleartext HTTP. The no-base path is left unchanged.
///
/// Returns the resolved [`Uri`] and the fragment (preserved separately because
/// `http::Uri` strips fragments per RFC 7230).
pub(crate) fn resolve_request_url(
    base: Option<&url::Url>,
    input: &str,
) -> Result<(Uri, Option<String>), Error> {
    match base {
        Some(base) => {
            let resolved = base
                .join(input)
                .map_err(|e| Error::InvalidUrl(format!("{e}")))?;
            if !matches!(resolved.scheme(), "http" | "https") {
                return Err(Error::InvalidUrl(format!(
                    "request URL scheme must be http or https, got `{}`",
                    resolved.scheme()
                )));
            }
            if resolved.host_str().is_none_or(|h| h.is_empty()) {
                return Err(Error::InvalidUrl("request URL must include a host".into()));
            }
            let fragment = resolved.fragment().map(|f| f.to_owned());
            let uri: Uri = resolved
                .as_str()
                .parse()
                .map_err(|e| Error::InvalidUrl(format!("{e}")))?;
            Ok((uri, fragment))
        }
        None => {
            let fragment = extract_fragment(input);
            let uri: Uri = input
                .parse()
                .map_err(|e| Error::InvalidUrl(format!("{e}")))?;
            Ok((uri, fragment))
        }
    }
}

fn resolve_redirect(
    base: &Uri,
    location: &str,
    original_fragment: Option<&str>,
) -> Result<(Uri, Option<String>), Error> {
    base.scheme_str()
        .ok_or_else(|| Error::InvalidUrl("missing scheme in base".into()))?;
    base.authority()
        .ok_or_else(|| Error::InvalidUrl("missing authority in base".into()))?;

    // Use url::Url for fragment-aware resolution. http::Uri strips fragments
    // per RFC 7230, so we pass the original fragment explicitly for the RFC 7231
    // Section 7.1.2 requirement: if the Location header has no fragment, the
    // original request's fragment MUST be inherited.
    let base_url =
        url::Url::parse(&base.to_string()).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    let mut next = base_url
        .join(location)
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))?;

    // Restrict the resolved target to http/https with a host. Without this, a
    // redirect to a non-http(s) absolute target (e.g. `ftp://host/path`) parses
    // into a valid `http::Uri` with an authority that dispatch then treats as
    // non-HTTPS and sends as cleartext HTTP to port 80.
    if !matches!(next.scheme(), "http" | "https") {
        return Err(Error::Redirect(format!(
            "redirect target scheme must be http or https, got `{}`",
            next.scheme()
        )));
    }
    if next.host_str().is_none_or(|h| h.is_empty()) {
        return Err(Error::Redirect(
            "redirect target must include a host".into(),
        ));
    }

    // Preserve original fragment when Location has none (RFC 7231 7.1.2).
    if next.fragment().is_none()
        && let Some(frag) = original_fragment
        && !frag.is_empty()
    {
        next.set_fragment(Some(frag));
    }

    let effective_fragment = next.fragment().map(|f| f.to_owned());
    let uri: Uri = next
        .as_str()
        .parse()
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))?;
    Ok((uri, effective_fragment))
}

fn boxed_response_from_bytes(
    status: StatusCode,
    headers: &HeaderMap,
    body: Bytes,
) -> http::Response<RequestBodySend> {
    let mut builder = http::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    // SAFETY: the builder uses a valid status code and headers that were
    // already validated when the response was originally built.
    #[allow(clippy::expect_used)]
    builder
        .body(
            http_body_util::Full::new(body)
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .expect("response builder with valid status cannot fail")
}

#[cfg(test)]
mod tests;
