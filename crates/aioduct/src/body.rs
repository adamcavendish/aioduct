#[cfg(not(target_arch = "wasm32"))]
use std::pin::Pin;

#[cfg(not(target_arch = "wasm32"))]
use crate::clock::Instant;

use bytes::Bytes;
use http_body_util::BodyExt;

use crate::error::Error;
#[cfg(not(target_arch = "wasm32"))]
use crate::observer::{self, RequestEvent, RequestPhase, TransferDirection};
#[cfg(not(target_arch = "wasm32"))]
use crate::response::BodyObserverCtx;

// ── Boxed body type aliases ──────────────────────────────────────────────────

/// Boxed request body (always `Send`).
///
/// Used as the body type for `http::Request<RequestBoxBody>` throughout the
/// dispatch pipeline. Based on `UnsyncBoxBody` from `http-body-util`.
pub type RequestBoxBody = http_body_util::combinators::UnsyncBoxBody<Bytes, Error>;

/// Boxed `Send` response body for poll-based runtimes (tokio, smol).
///
/// This is the default body type for [`Response`](crate::response::Response).
/// It can hold either a raw hyper `Incoming` body or a type-erased boxed body
/// after transformations (decompression, read timeout, bandwidth limiting).
#[cfg(not(target_arch = "wasm32"))]
pub type ResponseBoxSendBody =
    Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + Send + 'static>>;

/// Boxed `!Send` response body for completion-based runtimes (compio).
///
/// Used as the body type for [`Response<ResponseBoxLocalBody>`](crate::response::Response)
/// in the Local path. Can wrap body transformations that contain `!Send` futures
/// (e.g., read timeout with a `!Send` sleep).
#[cfg(not(target_arch = "wasm32"))]
pub type ResponseBoxLocalBody =
    Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + 'static>>;

// ── Request body enum ────────────────────────────────────────────────────────

/// HTTP request body, either buffered in memory or streaming.
pub enum RequestBody {
    /// Fully buffered body from bytes.
    Buffered(Bytes),
    /// Streaming body from a boxed hyper body.
    #[cfg(not(target_arch = "wasm32"))]
    Streaming(RequestBoxBody),
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
    pub(crate) fn into_hyper_body(self) -> RequestBoxBody {
        match self {
            RequestBody::Buffered(b) => http_body_util::Full::new(b)
                .map_err(|never| match never {})
                .boxed_unsync(),
            #[cfg(not(target_arch = "wasm32"))]
            RequestBody::Streaming(body) => body,
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
impl From<RequestBoxBody> for RequestBody {
    fn from(body: RequestBoxBody) -> Self {
        RequestBody::Streaming(body)
    }
}

/// Async iterator over response body data frames.
pub struct BodyStream {
    body: RequestBoxBody,
    done: bool,
    #[cfg(not(target_arch = "wasm32"))]
    observer_ctx: Option<BodyObserverCtx>,
    #[cfg(not(target_arch = "wasm32"))]
    cumulative_bytes: u64,
    #[cfg(not(target_arch = "wasm32"))]
    transfer_start: Instant,
}

impl std::fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStream").finish()
    }
}

impl BodyStream {
    #[cfg(test)]
    pub(crate) fn new(body: RequestBoxBody) -> Self {
        Self {
            body,
            done: false,
            #[cfg(not(target_arch = "wasm32"))]
            observer_ctx: None,
            #[cfg(not(target_arch = "wasm32"))]
            cumulative_bytes: 0,
            #[cfg(not(target_arch = "wasm32"))]
            transfer_start: Instant::now(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn with_observer(body: RequestBoxBody, ctx: Option<BodyObserverCtx>) -> Self {
        let transfer_start = ctx
            .as_ref()
            .map(|c| c.response_started)
            .unwrap_or_else(Instant::now);
        Self {
            body,
            done: false,
            observer_ctx: ctx,
            cumulative_bytes: 0,
            transfer_start,
        }
    }

    /// Returns the next chunk of body data, or `None` when complete.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        if self.done {
            return None;
        }

        loop {
            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            let chunk_bytes = data.len() as u64;
                            self.cumulative_bytes += chunk_bytes;
                            if let Some(ctx) = &self.observer_ctx {
                                ctx.observer.on_event(&RequestEvent {
                                    method: ctx.method.clone(),
                                    uri: ctx.uri.clone(),
                                    phase: RequestPhase::BytesTransferred {
                                        direction: TransferDirection::Download,
                                        chunk_bytes,
                                        cumulative_bytes: self.cumulative_bytes,
                                        elapsed: self.transfer_start.elapsed(),
                                    },
                                    at: observer::Instant::now(),
                                });
                            }
                        }
                        return Some(Ok(data));
                    }
                }
                Some(Err(e)) => {
                    self.done = true;
                    #[cfg(not(target_arch = "wasm32"))]
                    self.fire_transfer_aborted(&e);
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                    #[cfg(not(target_arch = "wasm32"))]
                    self.fire_transfer_complete();
                    return None;
                }
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn fire_transfer_complete(&self) {
        if let Some(ctx) = &self.observer_ctx {
            let transfer_duration = self.transfer_start.elapsed();
            let throughput = if transfer_duration.as_secs_f64() > 0.0 {
                (self.cumulative_bytes as f64 / transfer_duration.as_secs_f64()) as f32
            } else {
                0.0
            };
            ctx.observer.on_event(&RequestEvent {
                method: ctx.method.clone(),
                uri: ctx.uri.clone(),
                phase: RequestPhase::TransferComplete {
                    direction: TransferDirection::Download,
                    total_bytes: self.cumulative_bytes,
                    transfer_duration,
                    throughput_bytes_per_sec: throughput,
                },
                at: observer::Instant::now(),
            });
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn fire_transfer_aborted(&self, error: &crate::error::Error) {
        if let Some(ctx) = &self.observer_ctx {
            ctx.observer.on_event(&RequestEvent {
                method: ctx.method.clone(),
                uri: ctx.uri.clone(),
                phase: RequestPhase::TransferAborted {
                    direction: TransferDirection::Download,
                    bytes_transferred: self.cumulative_bytes,
                    elapsed: self.transfer_start.elapsed(),
                    error: error.to_string(),
                },
                at: observer::Instant::now(),
            });
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn buffered(data: &[u8]) -> RequestBody {
        RequestBody::Buffered(Bytes::from(data.to_vec()))
    }

    fn streaming() -> RequestBody {
        let body: RequestBoxBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        RequestBody::Streaming(body)
    }

    #[test]
    fn try_clone_buffered_returns_some() {
        let body = buffered(b"hello");
        let cloned = body.try_clone();
        assert!(cloned.is_some());
        match cloned.unwrap() {
            RequestBody::Buffered(b) => assert_eq!(&b[..], b"hello"),
            _ => panic!("expected Buffered"),
        }
    }

    #[test]
    fn try_clone_streaming_returns_none() {
        let body = streaming();
        assert!(body.try_clone().is_none());
    }

    #[test]
    fn from_bytes() {
        let body: RequestBody = Bytes::from_static(b"data").into();
        match body {
            RequestBody::Buffered(b) => assert_eq!(&b[..], b"data"),
            _ => panic!("expected Buffered"),
        }
    }

    #[test]
    fn from_vec() {
        let body: RequestBody = vec![1u8, 2, 3].into();
        match body {
            RequestBody::Buffered(b) => assert_eq!(&b[..], &[1, 2, 3]),
            _ => panic!("expected Buffered"),
        }
    }

    #[test]
    fn from_string() {
        let body: RequestBody = String::from("text").into();
        match body {
            RequestBody::Buffered(b) => assert_eq!(&b[..], b"text"),
            _ => panic!("expected Buffered"),
        }
    }

    #[test]
    fn from_static_str() {
        let body: RequestBody = "static".into();
        match body {
            RequestBody::Buffered(b) => assert_eq!(&b[..], b"static"),
            _ => panic!("expected Buffered"),
        }
    }

    #[test]
    fn from_static_bytes() {
        let body: RequestBody = (b"bytes" as &'static [u8]).into();
        match body {
            RequestBody::Buffered(b) => assert_eq!(&b[..], b"bytes"),
            _ => panic!("expected Buffered"),
        }
    }

    #[test]
    fn from_hyper_body_is_streaming() {
        let hyper_body: RequestBoxBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let body: RequestBody = hyper_body.into();
        assert!(body.try_clone().is_none());
    }

    #[test]
    fn debug_buffered() {
        let body = buffered(b"data");
        let dbg = format!("{body:?}");
        assert!(dbg.contains("Buffered"));
    }

    #[test]
    fn debug_streaming() {
        let body = streaming();
        let dbg = format!("{body:?}");
        assert!(dbg.contains("Streaming"));
    }

    #[test]
    fn into_hyper_body_buffered() {
        let body = buffered(b"hello");
        let _hyper = body.into_hyper_body();
    }

    #[test]
    fn into_hyper_body_streaming() {
        let body = streaming();
        let _hyper = body.into_hyper_body();
    }

    #[test]
    fn body_stream_debug() {
        let hyper_body: RequestBoxBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let stream = BodyStream::new(hyper_body);
        let dbg = format!("{stream:?}");
        assert!(dbg.contains("BodyStream"));
    }

    #[tokio::test]
    async fn body_stream_empty_returns_none() {
        let hyper_body: RequestBoxBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStream::new(hyper_body);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn body_stream_with_data() {
        let hyper_body: RequestBoxBody = http_body_util::Full::new(Bytes::from("hello"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStream::new(hyper_body);
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"hello");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn body_stream_done_stays_none() {
        let hyper_body: RequestBoxBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStream::new(hyper_body);
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none());
    }
}
