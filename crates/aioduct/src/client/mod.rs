mod builder;
mod builder_build;
mod builder_build_local;
mod builder_setters;
mod connect;
mod connect_local;
mod dispatch;
mod dispatch_send;
mod engine_local;
mod engine_send;
mod execute;
mod execute_local;
mod execute_send;
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
use http::{StatusCode, Uri};
use http_body_util::BodyExt;

use crate::body::RequestBoxBody;
use crate::cache::HttpCache;
use crate::cookie::CookieJar;
use crate::error::Error;
use crate::h2c_probe::H2cProbeCache;
use crate::http2::Http2Config;
use crate::middleware::MiddlewareStack;
use crate::pool::ConnectionPool;
use crate::proxy::ProxySettings;
use crate::redirect::RedirectPolicy;
use crate::retry::RetryConfig;
use crate::runtime::Resolve;

const DEFAULT_USER_AGENT: &str = concat!("aioduct/", env!("CARGO_PKG_VERSION"));

/// HTTP client with connection pooling, TLS, and automatic redirect handling.
///
/// This type supports both poll-based runtimes (tokio, smol) via [`RuntimePoll`](crate::runtime::RuntimePoll) bounds
/// and completion-based runtimes (compio) via [`RuntimeLocal`](crate::runtime::RuntimeLocal) bounds.
pub struct HttpEngine<R, C> {
    pub(crate) pool: ConnectionPool,
    pub(crate) connector: C,
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
    pub(crate) h2c_probe_cache: H2cProbeCache,
    pub(crate) connection_coalescing: bool,
    pub(crate) observer: Option<Arc<dyn crate::observer::RequestObserver>>,
    #[cfg(feature = "tower")]
    pub(crate) tower_connector: Option<crate::connector::TowerConnectorSlot>,
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
    pub(crate) _phantom: PhantomData<(R, C)>,
}

impl<R, C: Clone> Clone for HttpEngine<R, C> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            connector: self.connector.clone(),
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
            h2c_probe_cache: self.h2c_probe_cache.clone(),
            connection_coalescing: self.connection_coalescing,
            observer: self.observer.clone(),
            #[cfg(feature = "tower")]
            tower_connector: self.tower_connector.clone(),
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
            _phantom: PhantomData,
        }
    }
}

impl<R, C> std::fmt::Debug for HttpEngine<R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEngine").finish()
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
) -> http::Response<RequestBoxBody> {
    let mut builder = http::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
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
