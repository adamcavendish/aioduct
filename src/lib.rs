#[cfg(not(any(feature = "tokio", feature = "smol", feature = "compio")))]
compile_error!("aioduct: enable at least one runtime feature: tokio, smol, or compio");

pub mod body;
pub mod client;
pub mod error;
pub mod multipart;
pub mod pool;
pub mod redirect;
pub mod request;
pub mod response;
pub mod retry;
pub mod runtime;
pub mod sse;
mod timeout;
pub mod tls;

#[cfg(feature = "http3")]
#[path = "h3/mod.rs"]
pub mod h3_transport;

pub use body::BodyStream;
pub use client::Client;
pub use error::Error;
pub use multipart::Multipart;
pub use redirect::{RedirectAction, RedirectPolicy};
pub use request::RequestBuilder;
pub use response::Response;
pub use retry::RetryConfig;
pub use runtime::Runtime;
pub use sse::{SseEvent, SseStream};

pub use http::{HeaderMap, Method, StatusCode, Version};
