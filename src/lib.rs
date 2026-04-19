//! Async-native HTTP client built directly on hyper 1.x.
//!
//! aioduct is runtime-agnostic: enable `tokio`, `smol`, or `compio` via feature flags.
//! For HTTPS, enable the `rustls` feature.

#[cfg(not(any(feature = "tokio", feature = "smol", feature = "compio", feature = "wasm")))]
compile_error!("aioduct: enable at least one runtime feature: tokio, smol, compio, or wasm");

pub mod body;
#[cfg(feature = "blocking")]
pub mod blocking;
pub mod cache;
pub mod chunk_download;
pub mod client;
#[cfg(feature = "tower")]
pub mod connector;
pub mod cookie;
pub mod error;
pub mod multipart;
pub mod pool;
pub mod proxy;
pub mod redirect;
pub mod request;
pub mod response;
pub mod retry;
pub mod runtime;
pub mod sse;
mod timeout;
pub mod tls;

mod decompress;
#[cfg(feature = "hickory-dns")]
pub mod hickory;
pub mod http2;
pub mod middleware;
mod socks4;
mod socks5;
pub mod throttle;
pub mod upgrade;

#[cfg(feature = "wasm")]
pub mod wasm;

#[cfg(feature = "http3")]
mod alt_svc;
#[cfg(feature = "http3")]
#[path = "h3/mod.rs"]
pub mod h3_transport;

pub use body::{BodyStream, RequestBody};
pub use cache::{CacheConfig, HttpCache};
pub use chunk_download::ChunkDownload;
pub use client::Client;
pub use cookie::CookieJar;
pub use error::{Error, HyperBody};
pub use http2::Http2Config;
pub use middleware::Middleware;
pub use multipart::{Multipart, Part};
pub use proxy::{NoProxy, ProxyConfig, ProxySettings};
pub use redirect::{RedirectAction, RedirectPolicy};
pub use request::RequestBuilder;
pub use response::Response;
pub use retry::{RetryBudget, RetryConfig};
pub use throttle::RateLimiter;
pub use runtime::{Resolve, Runtime};
#[cfg(feature = "hickory-dns")]
pub use hickory::HickoryResolver;
pub use sse::{SseEvent, SseStream};
pub use upgrade::Upgraded;

pub use tls::TlsVersion;
pub use tls::TlsInfo;
#[cfg(feature = "rustls")]
pub use tls::{Certificate, Identity};

pub use http::{HeaderMap, Method, StatusCode, Uri, Version};
