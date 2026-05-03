//! Async-native HTTP client built directly on hyper 1.x.
//!
//! aioduct is runtime-agnostic: enable `tokio`, `smol`, or `compio` via feature flags.
//! For HTTPS, enable the `rustls` feature.

#![deny(missing_docs)]

#[cfg(not(any(
    feature = "tokio",
    feature = "smol",
    feature = "compio",
    feature = "wasm",
    doc
)))]
compile_error!("aioduct: enable at least one runtime feature: tokio, smol, compio, or wasm");

#[cfg(all(feature = "http3", not(feature = "rustls")))]
compile_error!("aioduct: the `http3` feature currently requires the `rustls` TLS backend feature");

/// Blocking (synchronous) HTTP client wrapper.
#[cfg(feature = "blocking")]
pub mod blocking;
/// Request and response body types.
#[cfg(not(target_arch = "wasm32"))]
pub mod body;
/// HTTP response caching with conditional validation.
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
/// Parallel range-request file downloader.
#[cfg(not(target_arch = "wasm32"))]
pub mod chunk_download;
/// HTTP client with connection pooling and redirect handling.
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
/// Tower-based connector layer support.
#[cfg(feature = "tower")]
pub mod connector;
/// Cookie storage and automatic cookie handling.
#[cfg(not(target_arch = "wasm32"))]
pub mod cookie;
/// Error types for HTTP operations.
pub mod error;
/// Multipart/form-data request body builder.
#[cfg(not(target_arch = "wasm32"))]
pub mod multipart;
/// Internal connection pool for HTTP keep-alive.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod pool;
/// HTTP and SOCKS proxy configuration.
#[cfg(not(target_arch = "wasm32"))]
pub mod proxy;
/// Redirect policy configuration.
#[cfg(not(target_arch = "wasm32"))]
pub mod redirect;
/// Request builder for configuring and sending HTTP requests.
#[cfg(not(target_arch = "wasm32"))]
pub mod request;
/// HTTP response type with status, headers, and body.
#[cfg(not(target_arch = "wasm32"))]
pub mod response;
/// Automatic retry with exponential backoff.
#[cfg(not(target_arch = "wasm32"))]
pub mod retry;
/// Async runtime abstraction layer.
#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;
/// Server-Sent Events (SSE) stream parser.
#[cfg(not(target_arch = "wasm32"))]
pub mod sse;
#[cfg(not(target_arch = "wasm32"))]
mod timeout;
/// Per-request timing breakdown (DNS, TCP, TLS, TTFB).
#[cfg(not(target_arch = "wasm32"))]
pub mod timing;
/// TLS configuration and connector types.
#[cfg(not(target_arch = "wasm32"))]
pub mod tls;

/// Token-bucket bandwidth limiter for throttling download throughput.
#[cfg(not(target_arch = "wasm32"))]
pub mod bandwidth;
#[cfg(not(target_arch = "wasm32"))]
mod decompress;
#[cfg(not(target_arch = "wasm32"))]
mod digest_auth;
/// Request forwarding for proxy/gateway use cases.
#[cfg(not(target_arch = "wasm32"))]
pub mod forward;
/// Forwarded header builder and parser (RFC 7239).
#[cfg(not(target_arch = "wasm32"))]
pub mod forwarded;
#[cfg(not(target_arch = "wasm32"))]
mod happy_eyeballs;
/// Hickory DNS resolver integration.
#[cfg(feature = "hickory-dns")]
pub mod hickory;
/// HSTS (HTTP Strict Transport Security) store.
#[cfg(not(target_arch = "wasm32"))]
pub mod hsts;
/// HTTP/2 connection configuration.
#[cfg(not(target_arch = "wasm32"))]
pub mod http2;
/// Link header parsing (RFC 8288).
#[cfg(not(target_arch = "wasm32"))]
pub mod link;
/// Request/response middleware trait and stack.
#[cfg(not(target_arch = "wasm32"))]
pub mod middleware;
/// Netrc credential file parsing and middleware.
#[cfg(not(target_arch = "wasm32"))]
pub mod netrc;
/// RFC 9457 Problem Details for HTTP APIs.
#[cfg(feature = "json")]
pub mod problem;
#[cfg(not(target_arch = "wasm32"))]
mod socks4;
#[cfg(not(target_arch = "wasm32"))]
mod socks5;
/// Token-bucket rate limiter for throttling requests.
#[cfg(not(target_arch = "wasm32"))]
pub mod throttle;
/// HTTP upgrade (e.g., WebSocket) support.
#[cfg(not(target_arch = "wasm32"))]
pub mod upgrade;

/// WebAssembly runtime support.
#[cfg(feature = "wasm")]
pub mod wasm;

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

#[cfg(not(target_arch = "wasm32"))]
pub use bandwidth::BandwidthLimiter;
#[cfg(not(target_arch = "wasm32"))]
pub use body::{BodyStream, RequestBody};
#[cfg(not(target_arch = "wasm32"))]
pub use cache::{CacheConfig, CacheEntry, CacheStore, HttpCache, InMemoryCacheStore};
#[cfg(not(target_arch = "wasm32"))]
pub use chunk_download::ChunkDownload;
#[cfg(not(target_arch = "wasm32"))]
pub use client::Client;
#[cfg(not(target_arch = "wasm32"))]
pub use cookie::{Cookie, CookieJar, SameSite};
pub use error::{AioductBody, Error};
#[cfg(not(target_arch = "wasm32"))]
pub use forward::ForwardBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use forwarded::ForwardedElement;
#[cfg(feature = "hickory-dns")]
pub use hickory::HickoryResolver;
#[cfg(not(target_arch = "wasm32"))]
pub use hsts::HstsStore;
#[cfg(not(target_arch = "wasm32"))]
pub use http2::Http2Config;
#[cfg(not(target_arch = "wasm32"))]
pub use link::Link;
#[cfg(not(target_arch = "wasm32"))]
pub use middleware::Middleware;
#[cfg(not(target_arch = "wasm32"))]
pub use multipart::{Multipart, Part};
#[cfg(not(target_arch = "wasm32"))]
pub use netrc::{Netrc, NetrcMiddleware};
#[cfg(feature = "json")]
pub use problem::ProblemDetails;
#[cfg(not(target_arch = "wasm32"))]
pub use proxy::{NoProxy, ProxyConfig, ProxySettings};
#[cfg(not(target_arch = "wasm32"))]
pub use redirect::{RedirectAction, RedirectPolicy};
#[cfg(not(target_arch = "wasm32"))]
pub use request::RequestBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use response::Response;
#[cfg(not(target_arch = "wasm32"))]
pub use retry::{RetryBudget, RetryConfig};
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{Resolve, Runtime};
#[cfg(not(target_arch = "wasm32"))]
pub use sse::{SseDecoder, SseEvent, SseMessage, SseStream};
#[cfg(not(target_arch = "wasm32"))]
pub use throttle::RateLimiter;
#[cfg(not(target_arch = "wasm32"))]
pub use timing::RequestTimings;
#[cfg(not(target_arch = "wasm32"))]
pub use upgrade::Upgraded;

#[cfg(not(target_arch = "wasm32"))]
pub use tls::TlsInfo;
#[cfg(not(target_arch = "wasm32"))]
pub use tls::TlsVersion;
#[cfg(feature = "rustls")]
pub use tls::{Certificate, Identity};

pub use http::{HeaderMap, Method, StatusCode, Uri, Version};
#[cfg(not(target_arch = "wasm32"))]
pub use hyper::ext::Protocol;
