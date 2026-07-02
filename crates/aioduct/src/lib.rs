//! Async-native HTTP client built directly on hyper 1.x.
//!
//! aioduct is runtime-agnostic: enable `tokio`, `smol`, or `compio` via feature flags.
//! For HTTPS, enable the `rustls` feature.

#![deny(missing_docs)]
#![deny(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]
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

/// Address-family preference for DNS-resolved connections.
pub mod address_family;
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
mod digest_fields;
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
/// RFC 9421 HTTP Message Signatures helpers.
pub mod message_signatures;
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
/// Credential resolver trait and built-in implementations.
pub mod proxy_credential;
/// Redirect policy configuration.
pub mod redirect;
/// Automatic retry with exponential backoff.
pub mod retry;
mod sha256;
/// Server-Sent Events (SSE) stream parser.
pub mod sse;
/// Token-bucket rate limiter for throttling requests.
pub mod throttle;
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

pub use address_family::AddressFamily;
pub use bandwidth::BandwidthLimiter;
#[cfg(not(target_arch = "wasm32"))]
pub use body::BodyStreamLocal;
pub use body::{BodyStreamSend, RequestBody};
pub use cache::{CacheConfig, CacheEntry, CacheStore, HttpCache, InMemoryCacheStore};
pub use cookie::{Cookie, CookieJar, SameSite};
pub use digest_fields::{
    CONTENT_DIGEST, insert_sha256_content_digest, sha256_content_digest_value,
    sha256_content_digest_value_from_digest,
};
pub use error::{Error, PoolError, PoolLimitError, PoolLimitKind, SendError};
pub use forwarded::ForwardedElement;
pub use hsts::HstsStore;
pub use http2::Http2Config;
pub use link::Link;
pub use message_signatures::{
    AcceptSignature, AcceptSignatureEntry, AcceptSignatureFulfillment, AcceptSignatureParams,
    MessageSignature, MessageSignatureAsyncSigner, MessageSignatureAsyncSigningFuture,
    MessageSignatureBase, MessageSignatureComponent, MessageSignatureComponentParameter,
    MessageSignatureConfig, MessageSignatureError, MessageSignatureHeaders,
    MessageSignatureLocalAsyncSigner, MessageSignatureLocalAsyncSigningFuture,
    MessageSignatureParams, MessageSignatureRequestContext, MessageSignatureResponseContext,
    MessageSignatureSigner, MessageSignatureVerificationInput, MessageSignatureVerificationPolicy,
    MessageSignatureVerifier,
};
pub use middleware::Middleware;
pub use multipart::{Multipart, Part};
pub use netrc::{Netrc, NetrcMiddleware};
pub use observer::{
    ConnectionEvent, ConnectionPhase, NegotiatedProtocol, PoolOutcome, RequestEvent,
    RequestObserver, RequestPhase, RetryKind, TransferDirection,
};
#[cfg(not(target_arch = "wasm32"))]
pub use pool::{PoolHostStats, PoolStats};
pub use proxy::{NoProxy, ProxyChain, ProxyConfig, ProxySettings};
pub use proxy_credential::{CompositeResolver, CredentialResolver, EnvCredentialResolver};
pub use redirect::{RedirectAction, RedirectPolicy};
pub use retry::{RetryBudget, RetryConfig, RetryContext, RetryDecision, RetryOutcome};
#[cfg(not(target_arch = "wasm32"))]
pub use sse::SseStreamLocal;
pub use sse::{SseDecoder, SseEvent, SseMessage, SseStream, SseStreamSend};
pub use throttle::RateLimiter;
pub use traits::{HttpClient, RequestBuilderExt, ResponseExt};

#[cfg(feature = "json")]
pub use problem::ProblemDetails;

// ── Re-exports: native-only ──────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub use chunk_download::ChunkDownload;
#[cfg(not(target_arch = "wasm32"))]
pub use chunk_download::ChunkDownloadLocal;
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
#[cfg(not(target_arch = "wasm32"))]
pub use forward::forward_local::ForwardBuilderLocal;
#[cfg(feature = "hickory-dns")]
pub use hickory::HickoryResolver;
#[cfg(not(target_arch = "wasm32"))]
pub use request::RequestBuilderLocal;
#[cfg(not(target_arch = "wasm32"))]
pub use request::RequestBuilderSend;

#[cfg(not(target_arch = "wasm32"))]
pub use response::Response;
#[cfg(any(feature = "tokio", feature = "smol", feature = "compio"))]
pub use runtime::SystemResolver;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{
    ConnectorLocal, ConnectorSend, FallbackResolver, Resolve, RuntimeCompletion, RuntimeLocal,
    RuntimePoll, SocketConfig, StaticResolver,
};
#[cfg(feature = "wasi-p2")]
pub use traits::OwnedWasiRequestBuilder;
#[cfg(feature = "wasm")]
pub use traits::OwnedWasmRequestBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use traits::{OwnedRequestBuilderLocal, OwnedRequestBuilderSend};

#[cfg(not(target_arch = "wasm32"))]
pub use upgrade::Upgraded;
#[cfg(not(target_arch = "wasm32"))]
pub use upgrade::UpgradedLocal;

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

/// Convenience alias for the WebAssembly (browser Fetch API) client.
#[cfg(feature = "wasm")]
pub type WasmClient = wasm::WasmClient;

/// Convenience alias for the WASI Preview 2 HTTP client.
#[cfg(feature = "wasi-p2")]
pub type WasiClient = wasi_p2::WasiClient;

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
#[allow(clippy::expect_used, clippy::unwrap_used)]
pub mod __bench {
    use std::net::{IpAddr, SocketAddr};
    use std::time::Duration;

    use crate::body::RequestBodySend;
    use crate::pool::{ConnectionPool, PoolKey, PooledConnection};
    use crate::runtime::TokioRuntime;
    use http::uri::{Authority, Scheme};

    pub struct BenchPool(ConnectionPool<RequestBodySend>);
    pub struct BenchConn(Option<PooledConnection<RequestBodySend>>);
    pub struct BenchKey(PoolKey);

    pub fn new_pool(max_idle: usize, timeout: Duration) -> BenchPool {
        BenchPool(
            ConnectionPool::new()
                .without_reaper()
                .with_max_idle_per_host(max_idle)
                .with_idle_timeout(timeout),
        )
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
            c.sans = std::sync::Arc::from(sans);
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
            .checkout_coalesced(target_host, resolved_ip, crate::pool::ProxyRoute::DIRECT)
            .is_some()
    }

    pub fn checkout(pool: &BenchPool, key: &BenchKey) -> Option<BenchConn> {
        pool.0.checkout(&key.0).map(|c| BenchConn(Some(c)))
    }

    pub fn wrap_read_timeout_body(
        body: crate::body::RequestBodySend,
        duration: Duration,
    ) -> crate::body::RequestBodySend {
        use http_body_util::BodyExt;
        crate::timeout::ReadTimeoutBody::<_, TokioRuntime>::new(body, duration)
            .map_err(|e| e)
            .boxed_unsync()
    }

    pub fn wrap_bandwidth_body(
        body: crate::body::RequestBodySend,
        limiter: crate::bandwidth::BandwidthLimiter,
    ) -> crate::body::RequestBodySend {
        use http_body_util::BodyExt;
        crate::bandwidth::BandwidthBody::<_, TokioRuntime>::new(body, limiter).boxed_unsync()
    }

    pub fn make_full_body(total_size: usize) -> crate::body::RequestBodySend {
        use http_body_util::BodyExt;
        http_body_util::Full::new(bytes::Bytes::from(vec![b'X'; total_size]))
            .map_err(|never| match never {})
            .boxed_unsync()
    }
}
