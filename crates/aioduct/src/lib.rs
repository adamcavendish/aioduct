//! Async-native HTTP client built directly on hyper 1.x.
//!
//! aioduct is runtime-agnostic: enable `tokio`, `smol`, or `compio` via feature flags.
//! For HTTPS, enable the `rustls` feature.

#![deny(missing_docs)]
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(not(any(
    feature = "tokio",
    feature = "smol",
    feature = "compio",
    feature = "wasm",
    feature = "wasi-p2",
    doc
)))]
compile_error!(
    "aioduct: enable at least one runtime feature: tokio, smol, compio, wasm, or wasi-p2"
);

#[cfg(all(feature = "http3", not(feature = "rustls")))]
compile_error!("aioduct: the `http3` feature currently requires the `rustls` TLS backend feature");

// ── Portable modules (available on all targets including wasm32) ─────────────

/// Token-bucket bandwidth limiter for throttling download throughput.
pub mod bandwidth;
/// Request and response body types.
pub mod body;
/// HTTP response caching with conditional validation.
pub mod cache;
mod clock;
/// Cookie storage and automatic cookie handling.
pub mod cookie;
mod decompress;
mod digest_auth;
/// Error types for HTTP operations.
pub mod error;
/// Forwarded header builder and parser (RFC 7239).
pub mod forwarded;
pub(crate) mod h2c_probe;
/// HSTS (HTTP Strict Transport Security) store.
pub mod hsts;
/// HTTP/2 connection configuration.
pub mod http2;
/// Link header parsing (RFC 8288).
pub mod link;
/// Request/response middleware trait and stack.
pub mod middleware;
/// Multipart/form-data request body builder.
pub mod multipart;
/// Netrc credential file parsing and middleware.
pub mod netrc;
/// Real-time request lifecycle observer for load testing and tracing.
pub mod observer;
/// HTTP and SOCKS proxy configuration.
pub mod proxy;
/// Redirect policy configuration.
pub mod redirect;
/// Automatic retry with exponential backoff.
pub mod retry;
/// Server-Sent Events (SSE) stream parser.
pub mod sse;
/// Token-bucket rate limiter for throttling requests.
pub mod throttle;
/// Per-request timing breakdown (DNS, TCP, TLS, TTFB).
///
/// Deprecated: Use [`observer::RequestObserver`] for detailed per-phase timing.
pub mod timing;
/// Consumer-facing client trait and extension traits.
pub mod traits;

/// RFC 9457 Problem Details for HTTP APIs.
#[cfg(feature = "json")]
pub mod problem;

// ── Native-only modules (require OS networking) ──────────────────────────────

/// Blocking (synchronous) HTTP client wrapper.
#[cfg(feature = "blocking")]
pub mod blocking;
/// Parallel range-request file downloader.
#[cfg(not(target_arch = "wasm32"))]
pub mod chunk_download;
/// HTTP client with connection pooling and redirect handling.
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
/// Tower-based connector layer support.
#[cfg(feature = "tower")]
pub mod connector;
/// Request forwarding for proxy/gateway use cases.
#[cfg(not(target_arch = "wasm32"))]
pub mod forward;
#[cfg(not(target_arch = "wasm32"))]
mod happy_eyeballs;
/// Hickory DNS resolver integration.
#[cfg(feature = "hickory-dns")]
pub mod hickory;
/// Internal connection pool for HTTP keep-alive.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod pool;
/// Request builder for configuring and sending HTTP requests.
#[cfg(not(target_arch = "wasm32"))]
pub mod request;
/// Request builder for `!Send` runtimes (compio, io_uring).
#[cfg(not(target_arch = "wasm32"))]
pub mod request_local;
/// HTTP response type with status, headers, and body.
#[cfg(not(target_arch = "wasm32"))]
pub mod response;
/// Async runtime abstraction layer.
#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;
#[cfg(not(target_arch = "wasm32"))]
mod socks4;
#[cfg(not(target_arch = "wasm32"))]
mod socks5;
#[cfg(not(target_arch = "wasm32"))]
mod timeout;
/// TLS configuration and connector types.
#[cfg(not(target_arch = "wasm32"))]
pub mod tls;
/// HTTP upgrade (e.g., WebSocket) support.
#[cfg(not(target_arch = "wasm32"))]
pub mod upgrade;

// ── Platform-specific client modules ─────────────────────────────────────────

/// WebAssembly (browser) runtime support.
#[cfg(feature = "wasm")]
pub mod wasm;

/// WASI Preview 2 HTTP client using wasi:http/outgoing-handler.
#[cfg(feature = "wasi-p2")]
pub mod wasi_p2;

#[cfg(feature = "tracing")]
mod tracing_middleware;
#[cfg(feature = "tracing")]
pub use tracing_middleware::TracingMiddleware;

#[cfg(feature = "otel")]
mod otel_middleware;
#[cfg(feature = "otel")]
pub use otel_middleware::OtelMiddleware;

#[cfg(all(feature = "http3", feature = "rustls"))]
mod alt_svc;
#[cfg(all(feature = "http3", feature = "rustls"))]
#[path = "h3/mod.rs"]
/// HTTP/3 transport layer using QUIC.
pub mod h3_transport;

// ── Re-exports: portable ─────────────────────────────────────────────────────

pub use bandwidth::BandwidthLimiter;
pub use body::{BodyStream, RequestBody};
pub use cache::{CacheConfig, CacheEntry, CacheStore, HttpCache, InMemoryCacheStore};
pub use cookie::{Cookie, CookieJar, SameSite};
pub use error::{Error, SendError};
pub use forwarded::ForwardedElement;
pub use hsts::HstsStore;
pub use http2::Http2Config;
pub use link::Link;
pub use middleware::Middleware;
pub use multipart::{Multipart, Part};
pub use netrc::{Netrc, NetrcMiddleware};
pub use observer::{
    ConnectionEvent, ConnectionPhase, NegotiatedProtocol, PoolOutcome, RequestEvent,
    RequestObserver, RequestPhase, TransferDirection,
};
pub use proxy::{NoProxy, ProxyConfig, ProxySettings};
pub use redirect::{RedirectAction, RedirectPolicy};
pub use retry::{RetryBudget, RetryConfig};
pub use sse::{SseDecoder, SseEvent, SseMessage, SseStream};
pub use throttle::RateLimiter;
#[allow(deprecated)]
pub use timing::RequestTimings;
pub use traits::{HttpClient, RequestBuilderExt, ResponseExt};

#[cfg(feature = "json")]
pub use problem::ProblemDetails;

// ── Re-exports: native-only ──────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub use chunk_download::ChunkDownload;
#[cfg(not(target_arch = "wasm32"))]
pub use client::HttpEngineBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use client::HttpEngineCore;
#[cfg(not(target_arch = "wasm32"))]
pub use client::HttpEngineLocal;
#[cfg(not(target_arch = "wasm32"))]
pub use client::HttpEngineSend;

#[cfg(not(target_arch = "wasm32"))]
pub use forward::ForwardBuilder;
#[cfg(feature = "hickory-dns")]
pub use hickory::HickoryResolver;
#[cfg(not(target_arch = "wasm32"))]
pub use request::RequestBuilderSend;
#[cfg(not(target_arch = "wasm32"))]
pub use request_local::RequestBuilderLocal;

#[cfg(not(target_arch = "wasm32"))]
#[deprecated(since = "0.2.0", note = "Renamed to `RequestBuilderSend`")]
/// Deprecated alias for [`RequestBuilderSend`].
pub type RequestBuilder<'a, R, C> = RequestBuilderSend<'a, R, C>;
#[cfg(not(target_arch = "wasm32"))]
pub use response::Response;
#[cfg(not(target_arch = "wasm32"))]
#[allow(deprecated)]
pub use runtime::Runtime;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{
    Connector, ConnectorSend, Resolve, RuntimeCompletion, RuntimeLocal, RuntimePoll, SocketConfig,
};
#[cfg(feature = "wasi-p2")]
pub use traits::OwnedWasiRequestBuilder;
#[cfg(feature = "wasm")]
pub use traits::OwnedWasmRequestBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use traits::{OwnedRequestBuilderLocal, OwnedRequestBuilderSend};

#[cfg(not(target_arch = "wasm32"))]
#[deprecated(since = "0.2.0", note = "Renamed to `OwnedRequestBuilderSend`")]
/// Deprecated alias for [`OwnedRequestBuilderSend`].
pub type OwnedRequestBuilder<R, C> = OwnedRequestBuilderSend<R, C>;
#[cfg(not(target_arch = "wasm32"))]
pub use upgrade::Upgraded;

/// Convenience alias for [`HttpEngineSend`] using the Tokio runtime.
#[cfg(feature = "tokio")]
pub type TokioClient =
    HttpEngineSend<runtime::tokio_rt::TokioRuntime, runtime::tokio_rt::TcpConnector>;

/// Alias for [`TokioClient`].
#[cfg(feature = "tokio")]
pub type TokioEngine = TokioClient;

/// Convenience alias for [`HttpEngineSend`] using the smol runtime.
#[cfg(feature = "smol")]
pub type SmolClient = HttpEngineSend<runtime::smol_rt::SmolRuntime, runtime::smol_rt::TcpConnector>;

/// Alias for [`SmolClient`].
#[cfg(feature = "smol")]
pub type SmolEngine = SmolClient;

/// Convenience alias for [`HttpEngineLocal`] using the compio runtime.
#[cfg(feature = "compio")]
pub type CompioClient =
    HttpEngineLocal<runtime::compio_rt::CompioRuntime, runtime::compio_rt::TcpConnector>;

/// Alias for [`CompioClient`].
#[cfg(feature = "compio")]
pub type CompioEngine = CompioClient;

/// Blocking client backed by the tokio runtime.
#[cfg(all(feature = "blocking", feature = "tokio"))]
pub type BlockingTokioClient =
    blocking::BlockingClient<TokioClient, runtime::tokio_rt::TokioRuntime>;

/// Blocking client backed by the smol runtime.
#[cfg(all(feature = "blocking", feature = "smol"))]
pub type BlockingSmolClient = blocking::BlockingClient<SmolClient, runtime::smol_rt::SmolRuntime>;

/// Blocking client backed by the compio runtime.
#[cfg(all(feature = "blocking", feature = "compio"))]
pub type BlockingCompioClient =
    blocking::BlockingClient<CompioClient, runtime::compio_rt::CompioRuntime>;

#[cfg(not(target_arch = "wasm32"))]
pub use tls::TlsInfo;
#[cfg(not(target_arch = "wasm32"))]
pub use tls::TlsVersion;
#[cfg(feature = "rustls")]
pub use tls::{Certificate, Identity};

pub use http::{HeaderMap, Method, StatusCode, Uri, Version};
#[cfg(not(target_arch = "wasm32"))]
pub use hyper::ext::Protocol;

#[cfg(feature = "__bench")]
#[doc(hidden)]
pub mod __bench {
    use std::net::{IpAddr, SocketAddr};
    use std::time::Duration;

    use crate::pool::{ConnectionPool, PoolKey, PooledConnection};
    use crate::runtime::TokioRuntime;
    use http::uri::{Authority, Scheme};

    pub struct BenchPool(ConnectionPool);
    pub struct BenchConn(Option<PooledConnection>);
    pub struct BenchKey(PoolKey);

    pub fn new_pool(max_idle: usize, timeout: Duration) -> BenchPool {
        BenchPool(ConnectionPool::new_no_reaper(max_idle, timeout))
    }

    pub async fn make_h2_conn() -> BenchConn {
        use crate::runtime::tokio_rt::TokioIo;
        let (client_io, server_io) = tokio::io::duplex(65536);

        tokio::spawn(async move {
            use hyper::server::conn::http2::Builder;
            use hyper::service::service_fn;
            let io = TokioIo::new(server_io);
            let _ = Builder::new(crate::runtime::executor::poll_executor::<TokioRuntime>())
                .serve_connection(
                    io,
                    service_fn(|_req| async {
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(
                            http_body_util::Empty::<bytes::Bytes>::new(),
                        ))
                    }),
                )
                .await;
        });

        let io = TokioIo::new(client_io);
        let (sender, conn) = hyper::client::conn::http2::handshake(
            crate::runtime::executor::poll_executor::<TokioRuntime>(),
            io,
        )
        .await
        .expect("h2 handshake");

        tokio::spawn(async move {
            let _ = conn.await;
        });

        BenchConn(Some(PooledConnection::new_h2(sender)))
    }

    pub fn pool_key(host: &str) -> BenchKey {
        BenchKey(PoolKey::new(
            Scheme::HTTPS,
            host.parse::<Authority>().unwrap(),
        ))
    }

    pub fn set_sans(conn: &mut BenchConn, sans: Vec<String>) {
        if let Some(c) = conn.0.as_mut() {
            c.sans = sans;
        }
    }

    pub fn set_remote_addr(conn: &mut BenchConn, addr: SocketAddr) {
        if let Some(c) = conn.0.as_mut() {
            c.remote_addr = Some(addr);
        }
    }

    pub fn checkin(pool: &BenchPool, key: BenchKey, conn: BenchConn) {
        if let Some(c) = conn.0 {
            pool.0.checkin(key.0, c);
        }
    }

    pub fn checkout_coalesced(
        pool: &BenchPool,
        target_host: &str,
        resolved_ip: Option<IpAddr>,
    ) -> bool {
        pool.0
            .checkout_coalesced(target_host, resolved_ip)
            .is_some()
    }
}
