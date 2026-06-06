mod builder;
mod builder_build;
mod builder_build_local;
mod builder_setters;
mod connect_handshake;
mod connect_protocol_local;
mod connect_protocol_send;
mod dispatch;
mod dispatch_send;
mod engine_local;
mod engine_send;
mod execute;
mod execute_local;
mod execute_send;
mod proxy_connect_local;
mod proxy_connect_send;
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

// ── Shared free functions ────────────────────────────────────────────────────

fn resolve_redirect(base: &Uri, location: &str) -> Result<Uri, Error> {
    base.scheme_str()
        .ok_or_else(|| Error::InvalidUrl("missing scheme in base".into()))?;
    base.authority()
        .ok_or_else(|| Error::InvalidUrl("missing authority in base".into()))?;

    let base_url =
        url::Url::parse(&base.to_string()).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    let next = base_url
        .join(location)
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))?;
    next.as_str()
        .parse()
        .map_err(|e| Error::InvalidUrl(format!("invalid redirect URL: {e}")))
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
