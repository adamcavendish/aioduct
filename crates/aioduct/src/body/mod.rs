//! HTTP request and response body types.
//!
//! This module provides the boxed body type aliases used throughout the
//! dispatch pipeline, the [`RequestBody`] enum (buffered vs streaming), and
//! the [`BodyStreamSend`] / [`BodyStreamLocal`] async byte-stream iterators.

mod stream;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

use bytes::Bytes;
use http_body_util::BodyExt;

#[cfg(not(target_arch = "wasm32"))]
use std::pin::Pin;

use crate::error::Error;

#[cfg(not(target_arch = "wasm32"))]
pub use stream::BodyStreamLocal;
pub use stream::BodyStreamSend;

// ── Boxed body type aliases ──────────────────────────────────────────────────

/// Boxed request body (always `Send`).
///
/// Used as the body type for `http::Request<RequestBodySend>` throughout the
/// dispatch pipeline. Based on `UnsyncBoxBody` from `http-body-util`.
pub type RequestBodySend = http_body_util::combinators::UnsyncBoxBody<Bytes, Error>;

/// Boxed `!Send` request body for completion-based runtimes (compio).
///
/// Used in the Local execution path where the body may contain `!Send` state
/// (e.g. compio futures). The body is created, polled, and dropped on the
/// same thread — no cross-thread migration occurs.
#[cfg(not(target_arch = "wasm32"))]
pub type RequestBodyLocal = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + 'static>>;

/// Boxed `!Send` response body for completion-based runtimes (compio).
///
/// Used as the body type for [`Response<ResponseBodyLocal>`](crate::response::Response)
/// in the Local path. Can wrap body transformations that contain `!Send` futures
/// (e.g., read timeout with a `!Send` sleep).
#[cfg(not(target_arch = "wasm32"))]
pub type ResponseBodyLocal = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + 'static>>;

// ── Request body enum ────────────────────────────────────────────────────────

/// HTTP request body, either buffered in memory or streaming.
pub enum RequestBody {
    /// Fully buffered body from bytes.
    Buffered(Bytes),
    /// Streaming body from a boxed hyper body.
    #[cfg(not(target_arch = "wasm32"))]
    Streaming(RequestBodySend),
}

impl std::fmt::Debug for RequestBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestBody::Buffered(_) => f.debug_tuple("Buffered").field(&"..").finish(),
            #[cfg(not(target_arch = "wasm32"))]
            RequestBody::Streaming(_) => f.debug_tuple("Streaming").field(&"..").finish(),
        }
    }
}

impl RequestBody {
    pub(crate) fn into_hyper_body(self) -> RequestBodySend {
        match self {
            RequestBody::Buffered(b) => http_body_util::Full::new(b)
                .map_err(|never| match never {})
                .boxed_unsync(),
            #[cfg(not(target_arch = "wasm32"))]
            RequestBody::Streaming(body) => body,
        }
    }

    /// Convert to a `!Send` local body for completion-based runtimes.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn into_local_body(self) -> RequestBodyLocal {
        match self {
            RequestBody::Buffered(b) => {
                Box::pin(http_body_util::Full::new(b).map_err(|never| match never {}))
            }
            RequestBody::Streaming(body) => Box::pin(body),
        }
    }

    /// Clone this body if it is buffered. Returns `None` for streaming bodies.
    pub fn try_clone(&self) -> Option<Self> {
        match self {
            RequestBody::Buffered(b) => Some(RequestBody::Buffered(b.clone())),
            #[cfg(not(target_arch = "wasm32"))]
            RequestBody::Streaming(_) => None,
        }
    }
}

impl From<Bytes> for RequestBody {
    fn from(b: Bytes) -> Self {
        RequestBody::Buffered(b)
    }
}

impl From<Vec<u8>> for RequestBody {
    fn from(v: Vec<u8>) -> Self {
        RequestBody::Buffered(Bytes::from(v))
    }
}

impl From<String> for RequestBody {
    fn from(s: String) -> Self {
        RequestBody::Buffered(Bytes::from(s))
    }
}

impl From<&'static str> for RequestBody {
    fn from(s: &'static str) -> Self {
        RequestBody::Buffered(Bytes::from_static(s.as_bytes()))
    }
}

impl From<&'static [u8]> for RequestBody {
    fn from(s: &'static [u8]) -> Self {
        RequestBody::Buffered(Bytes::from_static(s))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<RequestBodySend> for RequestBody {
    fn from(body: RequestBodySend) -> Self {
        RequestBody::Streaming(body)
    }
}
