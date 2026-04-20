//! Async-native HTTP client built directly on hyper 1.x.
//!
//! aioduct is runtime-agnostic: enable `tokio`, `smol`, or `compio` via feature flags.
//! For HTTPS, enable the `rustls` feature.

#![deny(missing_docs)]

#[cfg(not(any(
    feature = "tokio",
    feature = "smol",
    feature = "compio",
    feature = "wasm"
)))]
compile_error!("aioduct: enable at least one runtime feature: tokio, smol, compio, or wasm");

/// Blocking (synchronous) HTTP client wrapper.
#[cfg(feature = "blocking")]
pub mod blocking;
/// Request and response body types.
pub mod body;
/// HTTP response caching with conditional validation.
pub mod cache;
/// Parallel range-request file downloader.
pub mod chunk_download;
/// HTTP client with connection pooling and redirect handling.
pub mod client;
/// Tower-based connector layer support.
#[cfg(feature = "tower")]
pub mod connector;
/// Cookie storage and automatic cookie handling.
pub mod cookie;
/// Error types for HTTP operations.
pub mod error;
/// Multipart/form-data request body builder.
pub mod multipart;
/// Internal connection pool for HTTP keep-alive.
pub(crate) mod pool;
/// HTTP and SOCKS proxy configuration.
pub mod proxy;
/// Redirect policy configuration.
pub mod redirect;
/// Request builder for configuring and sending HTTP requests.
pub mod request;
/// HTTP response type with status, headers, and body.
pub mod response;
/// Automatic retry with exponential backoff.
pub mod retry;
/// Async runtime abstraction layer.
pub mod runtime;
/// Server-Sent Events (SSE) stream parser.
pub mod sse;
mod timeout;
/// TLS configuration and connector types.
pub mod tls;

/// Token-bucket bandwidth limiter for throttling download throughput.
pub mod bandwidth;
mod decompress;
mod digest_auth;
/// Forwarded header builder and parser (RFC 7239).
pub mod forwarded;
mod happy_eyeballs;
/// Hickory DNS resolver integration.
#[cfg(feature = "hickory-dns")]
pub mod hickory;
/// HSTS (HTTP Strict Transport Security) store.
pub mod hsts;
/// HTTP/2 connection configuration.
pub mod http2;
/// Link header parsing (RFC 8288).
pub mod link;
/// Request/response middleware trait and stack.
pub mod middleware;
/// Netrc credential file parsing and middleware.
pub mod netrc;
/// RFC 9457 Problem Details for HTTP APIs.
#[cfg(feature = "json")]
pub mod problem;
mod socks4;
mod socks5;
/// Token-bucket rate limiter for throttling requests.
pub mod throttle;
/// HTTP upgrade (e.g., WebSocket) support.
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

#[cfg(feature = "http3")]
mod alt_svc;
#[cfg(feature = "http3")]
#[path = "h3/mod.rs"]
/// HTTP/3 transport layer using QUIC.
pub mod h3_transport;

pub use bandwidth::BandwidthLimiter;
pub use body::{BodyStream, RequestBody};
pub use cache::{CacheConfig, HttpCache};
pub use chunk_download::ChunkDownload;
pub use client::Client;
pub use cookie::{Cookie, CookieJar, SameSite};
pub use error::{AioductBody, Error};
pub use forwarded::ForwardedElement;
#[cfg(feature = "hickory-dns")]
pub use hickory::HickoryResolver;
pub use hsts::HstsStore;
pub use http2::Http2Config;
pub use link::Link;
pub use middleware::Middleware;
pub use multipart::{Multipart, Part};
pub use netrc::{Netrc, NetrcMiddleware};
#[cfg(feature = "json")]
pub use problem::ProblemDetails;
pub use proxy::{NoProxy, ProxyConfig, ProxySettings};
pub use redirect::{RedirectAction, RedirectPolicy};
pub use request::RequestBuilder;
pub use response::Response;
pub use retry::{RetryBudget, RetryConfig};
pub use runtime::{Resolve, Runtime};
pub use sse::{SseEvent, SseStream};
pub use throttle::RateLimiter;
pub use upgrade::Upgraded;

pub use tls::TlsInfo;
pub use tls::TlsVersion;
#[cfg(feature = "rustls")]
pub use tls::{Certificate, Identity};

pub use http::{HeaderMap, Method, StatusCode, Uri, Version};
