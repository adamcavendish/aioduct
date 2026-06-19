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

/// Async iterator over response body data frames.
pub struct BodyStreamSend {
    body: RequestBodySend,
    done: bool,
    /// Trailer headers captured from the body's trailer frame, if any.
    trailers: Option<http::HeaderMap>,
    #[cfg(not(target_arch = "wasm32"))]
    observer_ctx: Option<BodyObserverCtx>,
    #[cfg(not(target_arch = "wasm32"))]
    cumulative_bytes: u64,
    #[cfg(not(target_arch = "wasm32"))]
    transfer_start: Instant,
}

impl std::fmt::Debug for BodyStreamSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStreamSend").finish()
    }
}

impl BodyStreamSend {
    #[cfg(test)]
    pub(crate) fn new(body: RequestBodySend) -> Self {
        Self {
            body,
            done: false,
            trailers: None,
            #[cfg(not(target_arch = "wasm32"))]
            observer_ctx: None,
            #[cfg(not(target_arch = "wasm32"))]
            cumulative_bytes: 0,
            #[cfg(not(target_arch = "wasm32"))]
            transfer_start: Instant::now(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn with_observer(body: RequestBodySend, ctx: Option<BodyObserverCtx>) -> Self {
        let transfer_start = ctx
            .as_ref()
            .map(|c| c.response_started)
            .unwrap_or_else(Instant::now);
        Self {
            body,
            done: false,
            trailers: None,
            observer_ctx: ctx,
            cumulative_bytes: 0,
            transfer_start,
        }
    }

    /// Returns the trailer headers received after the body, if any.
    ///
    /// Only populated once the stream has been fully consumed (i.e. `next()`
    /// returned `None`); trailers arrive after the final data frame.
    pub fn trailers(&self) -> Option<&http::HeaderMap> {
        self.trailers.as_ref()
    }

    /// Returns the next chunk of body data, or `None` when complete.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        if self.done {
            return None;
        }

        loop {
            match self.body.frame().await {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        // Skip empty data frames: a consumer expects `Some` to
                        // carry non-empty data and `None` to mean end of stream.
                        if data.is_empty() {
                            continue;
                        }
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
                    Err(frame) => {
                        // Capture trailers so callers can read them via
                        // `trailers()` after the stream completes.
                        match frame.into_trailers() {
                            Ok(trailers) => {
                                #[cfg(not(target_arch = "wasm32"))]
                                if let Some(ctx) = &self.observer_ctx {
                                    let headers: Vec<(String, String)> = trailers
                                        .iter()
                                        .map(|(k, v)| {
                                            (
                                                k.as_str().to_owned(),
                                                v.to_str().unwrap_or("<binary>").to_owned(),
                                            )
                                        })
                                        .collect();
                                    ctx.observer.on_event(&RequestEvent {
                                        method: ctx.method.clone(),
                                        uri: ctx.uri.clone(),
                                        phase: RequestPhase::TrailersReceived { headers },
                                        at: observer::Instant::now(),
                                    });
                                }
                                match &mut self.trailers {
                                    Some(existing) => existing.extend(trailers),
                                    None => self.trailers = Some(trailers),
                                }
                            }
                            Err(_non_trailer) => {}
                        }
                    }
                },
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

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BodyStreamSend {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            self.fire_transfer_aborted(&crate::error::Error::Other("body stream dropped".into()));
        }
    }
}

/// Async iterator over response body data frames for `!Send` bodies.
///
/// Used by completion-based runtimes (compio) where the response body is not `Send`.
#[cfg(not(target_arch = "wasm32"))]
pub struct BodyStreamLocal {
    body: ResponseBodyLocal,
    done: bool,
    trailers: Option<http::HeaderMap>,
    observer_ctx: Option<BodyObserverCtx>,
    cumulative_bytes: u64,
    transfer_start: Instant,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for BodyStreamLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStreamLocal").finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl BodyStreamLocal {
    pub(crate) fn with_observer(body: ResponseBodyLocal, ctx: Option<BodyObserverCtx>) -> Self {
        let transfer_start = ctx
            .as_ref()
            .map(|c| c.response_started)
            .unwrap_or_else(Instant::now);
        Self {
            body,
            done: false,
            trailers: None,
            observer_ctx: ctx,
            cumulative_bytes: 0,
            transfer_start,
        }
    }

    /// Returns the trailer headers received after the body, if any.
    ///
    /// Only populated once the stream has been fully consumed.
    pub fn trailers(&self) -> Option<&http::HeaderMap> {
        self.trailers.as_ref()
    }

    /// Returns the next chunk of body data, or `None` when complete.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        use std::pin::Pin;

        if self.done {
            return None;
        }

        loop {
            match Pin::new(&mut self.body).frame().await {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        // Skip empty data frames: a consumer expects `Some` to
                        // carry non-empty data and `None` to mean end of stream.
                        if data.is_empty() {
                            continue;
                        }
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
                        return Some(Ok(data));
                    }
                    Err(frame) => match frame.into_trailers() {
                        Ok(trailers) => {
                            if let Some(ctx) = &self.observer_ctx {
                                let headers: Vec<(String, String)> = trailers
                                    .iter()
                                    .map(|(k, v)| {
                                        (
                                            k.as_str().to_owned(),
                                            v.to_str().unwrap_or("<binary>").to_owned(),
                                        )
                                    })
                                    .collect();
                                ctx.observer.on_event(&RequestEvent {
                                    method: ctx.method.clone(),
                                    uri: ctx.uri.clone(),
                                    phase: RequestPhase::TrailersReceived { headers },
                                    at: observer::Instant::now(),
                                });
                            }
                            match &mut self.trailers {
                                Some(existing) => existing.extend(trailers),
                                None => self.trailers = Some(trailers),
                            }
                        }
                        Err(_non_trailer) => {}
                    },
                },
                Some(Err(e)) => {
                    self.done = true;
                    self.fire_transfer_aborted(&e);
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                    self.fire_transfer_complete();
                    return None;
                }
            }
        }
    }

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

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BodyStreamLocal {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            self.fire_transfer_aborted(&crate::error::Error::Other("body stream dropped".into()));
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
        let body: RequestBodySend = http_body_util::Empty::new()
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
        let hyper_body: RequestBodySend = http_body_util::Empty::new()
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
    fn body_stream_debug() {
        let hyper_body: RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let stream = BodyStreamSend::new(hyper_body);
        let dbg = format!("{stream:?}");
        assert!(dbg.contains("BodyStreamSend"));
    }

    #[tokio::test]
    async fn body_stream_empty_returns_none() {
        let hyper_body: RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStreamSend::new(hyper_body);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn body_stream_with_data() {
        let hyper_body: RequestBodySend = http_body_util::Full::new(Bytes::from("hello"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStreamSend::new(hyper_body);
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"hello");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn body_stream_done_stays_none() {
        let hyper_body: RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStreamSend::new(hyper_body);
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn into_local_body_buffered() {
        let body = buffered(b"local_test");
        let local = body.into_local_body();
        // Verify it's a valid body by checking size_hint
        use http_body::Body;
        let hint = local.size_hint();
        assert_eq!(hint.exact(), Some(10));
    }

    #[test]
    fn into_local_body_streaming() {
        let body = streaming();
        let local = body.into_local_body();
        // Streaming empty body has size 0
        use http_body::Body;
        let hint = local.size_hint();
        assert_eq!(hint.exact(), Some(0));
    }

    #[tokio::test]
    async fn body_stream_error_propagates_and_marks_done() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct ErrorAfterFirst {
            sent: bool,
        }

        impl http_body::Body for ErrorAfterFirst {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                if !self.sent {
                    self.sent = true;
                    Poll::Ready(Some(Err(Error::Other("deliberate error".into()))))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let hyper_body: RequestBodySend = ErrorAfterFirst { sent: false }.boxed_unsync();
        let mut stream = BodyStreamSend::new(hyper_body);

        // First call should return the error
        let result = stream.next().await;
        assert!(result.is_some());
        assert!(result.unwrap().is_err());

        // After error, stream is done
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn body_stream_skips_non_data_frames() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct TrailerThenData {
            state: u8,
        }

        impl http_body::Body for TrailerThenData {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                match self.state {
                    0 => {
                        self.state = 1;
                        // Emit a trailers frame (non-data)
                        let mut trailers = http::HeaderMap::new();
                        trailers.insert("x-test", "val".parse().unwrap());
                        Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
                    }
                    1 => {
                        self.state = 2;
                        Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from(
                            "after_trailer",
                        )))))
                    }
                    _ => Poll::Ready(None),
                }
            }
        }

        let hyper_body: RequestBodySend = TrailerThenData { state: 0 }.boxed_unsync();
        let mut stream = BodyStreamSend::new(hyper_body);

        // The trailers frame should be skipped, only data returned
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"after_trailer");

        // Then done
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn body_stream_exposes_trailers_after_drain() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        // Realistic order: data frame, then a trailers frame, then end.
        struct DataThenTrailer {
            state: u8,
        }
        impl http_body::Body for DataThenTrailer {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                match self.state {
                    0 => {
                        self.state = 1;
                        Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from("payload")))))
                    }
                    1 => {
                        self.state = 2;
                        let mut t = http::HeaderMap::new();
                        t.insert("x-checksum", "abc123".parse().unwrap());
                        Poll::Ready(Some(Ok(http_body::Frame::trailers(t))))
                    }
                    _ => Poll::Ready(None),
                }
            }
        }

        let body: RequestBodySend = DataThenTrailer { state: 0 }.boxed_unsync();
        let mut stream = BodyStreamSend::new(body);

        // Trailers are not available until the body is fully drained.
        assert!(stream.trailers().is_none());
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"payload");
        assert!(stream.next().await.is_none());

        let trailers = stream.trailers().expect("trailers should be captured");
        assert_eq!(trailers.get("x-checksum").unwrap(), "abc123");
    }

    #[tokio::test]
    async fn body_stream_no_trailers_stays_none() {
        use http_body_util::BodyExt;
        let body: RequestBodySend = http_body_util::Full::new(Bytes::from("no trailers"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStreamSend::new(body);
        while stream.next().await.is_some() {}
        assert!(stream.trailers().is_none());
    }

    #[tokio::test]
    async fn body_stream_skips_empty_data_frames() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        // A body that emits: empty, "hello", empty, then end. The empty data
        // frames must not surface as `Some(Ok(b""))` — a consumer expects
        // `Some` to mean non-empty data and `None` to mean end of stream.
        struct EmptyThenData {
            state: u8,
        }

        impl http_body::Body for EmptyThenData {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                let frame = match self.state {
                    0 => http_body::Frame::data(Bytes::new()),
                    1 => http_body::Frame::data(Bytes::from("hello")),
                    2 => http_body::Frame::data(Bytes::new()),
                    _ => return Poll::Ready(None),
                };
                self.state += 1;
                Poll::Ready(Some(Ok(frame)))
            }
        }

        let hyper_body: RequestBodySend = EmptyThenData { state: 0 }.boxed_unsync();
        let mut stream = BodyStreamSend::new(hyper_body);

        // Only the non-empty chunk is yielded.
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"hello");

        // The trailing empty frame is skipped; stream terminates with None.
        assert!(
            stream.next().await.is_none(),
            "empty data frames must not be yielded as empty chunks"
        );
    }

    #[tokio::test]
    async fn body_stream_with_observer_fires_transfer_events() {
        use std::sync::{Arc, Mutex};

        #[derive(Default, Clone)]
        struct TestObs {
            events: Arc<Mutex<Vec<String>>>,
        }

        impl observer::RequestObserver for TestObs {
            fn on_event(&self, event: &observer::RequestEvent) {
                let name = match &event.phase {
                    RequestPhase::BytesTransferred {
                        chunk_bytes,
                        cumulative_bytes,
                        ..
                    } => {
                        format!("BytesTransferred(chunk={chunk_bytes},cum={cumulative_bytes})")
                    }
                    RequestPhase::TransferComplete { total_bytes, .. } => {
                        format!("TransferComplete(total={total_bytes})")
                    }
                    RequestPhase::TransferAborted {
                        bytes_transferred, ..
                    } => {
                        format!("TransferAborted(bytes={bytes_transferred})")
                    }
                    other => format!("{other:?}"),
                };
                self.events.lock().unwrap().push(name);
            }

            fn on_connection_event(&self, _event: &observer::ConnectionEvent) {}
        }

        let obs = TestObs::default();
        let ctx = BodyObserverCtx {
            observer: Arc::new(obs.clone()),
            method: http::Method::GET,
            uri: "http://example.com/test".parse().unwrap(),
            response_started: Instant::now(),
        };

        let hyper_body: RequestBodySend = http_body_util::Full::new(Bytes::from("hello world"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStreamSend::with_observer(hyper_body, Some(ctx));

        // Read all data
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"hello world");

        // Should get None (triggers TransferComplete)
        assert!(stream.next().await.is_none());

        let events = obs.events.lock().unwrap();
        assert!(
            events.iter().any(|e| e.contains("BytesTransferred")),
            "should fire BytesTransferred, got: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.contains("TransferComplete(total=11)")),
            "should fire TransferComplete with 11 bytes, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn body_stream_with_observer_fires_transfer_aborted_on_error() {
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};
        use std::task::{Context, Poll};

        #[derive(Default, Clone)]
        struct TestObs {
            events: Arc<Mutex<Vec<String>>>,
        }

        impl observer::RequestObserver for TestObs {
            fn on_event(&self, event: &observer::RequestEvent) {
                let name = match &event.phase {
                    RequestPhase::TransferAborted {
                        bytes_transferred,
                        error,
                        ..
                    } => {
                        format!("TransferAborted(bytes={bytes_transferred},err={error})")
                    }
                    RequestPhase::BytesTransferred { .. } => "BytesTransferred".into(),
                    other => format!("{other:?}"),
                };
                self.events.lock().unwrap().push(name);
            }

            fn on_connection_event(&self, _event: &observer::ConnectionEvent) {}
        }

        struct ErrorBody;
        impl http_body::Body for ErrorBody {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                Poll::Ready(Some(Err(Error::Other("test error".into()))))
            }
        }

        let obs = TestObs::default();
        let ctx = BodyObserverCtx {
            observer: Arc::new(obs.clone()),
            method: http::Method::GET,
            uri: "http://example.com/err".parse().unwrap(),
            response_started: Instant::now(),
        };

        let hyper_body: RequestBodySend = ErrorBody.boxed_unsync();
        let mut stream = BodyStreamSend::with_observer(hyper_body, Some(ctx));

        // Should get error
        let result = stream.next().await;
        assert!(result.is_some());
        assert!(result.unwrap().is_err());

        // Stream is done
        assert!(stream.next().await.is_none());

        let events = obs.events.lock().unwrap();
        assert!(
            events.iter().any(|e| e.contains("TransferAborted")),
            "should fire TransferAborted on error, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn body_stream_without_observer_still_works() {
        let hyper_body: RequestBodySend = http_body_util::Full::new(Bytes::from("no observer"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let mut stream = BodyStreamSend::with_observer(hyper_body, None);

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(&chunk[..], b"no observer");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn body_stream_trailers_fires_observer_event() {
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};
        use std::task::{Context, Poll};

        #[derive(Default, Clone)]
        struct TestObs {
            events: Arc<Mutex<Vec<String>>>,
        }

        impl observer::RequestObserver for TestObs {
            fn on_event(&self, event: &observer::RequestEvent) {
                let name = match &event.phase {
                    RequestPhase::TrailersReceived { headers } => {
                        let hdr_strs: Vec<String> =
                            headers.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        format!("TrailersReceived({})", hdr_strs.join(","))
                    }
                    other => format!("{other:?}"),
                };
                self.events.lock().unwrap().push(name);
            }

            fn on_connection_event(&self, _event: &observer::ConnectionEvent) {}
        }

        struct TrailerOnlyBody {
            sent: bool,
        }

        impl http_body::Body for TrailerOnlyBody {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                if !self.sent {
                    self.sent = true;
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("x-trailer", "trailer-val".parse().unwrap());
                    Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let obs = TestObs::default();
        let ctx = BodyObserverCtx {
            observer: Arc::new(obs.clone()),
            method: http::Method::GET,
            uri: "http://example.com/trailers".parse().unwrap(),
            response_started: Instant::now(),
        };

        let hyper_body: RequestBodySend = TrailerOnlyBody { sent: false }.boxed_unsync();
        let mut stream = BodyStreamSend::with_observer(hyper_body, Some(ctx));

        // next() loops past the trailers frame (firing TrailersReceived)
        // and returns None when the body ends (firing TransferComplete)
        assert!(stream.next().await.is_none());

        let events = obs.events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.contains("TrailersReceived") && e.contains("x-trailer=trailer-val")),
            "should fire TrailersReceived with x-trailer=trailer-val, got: {events:?}"
        );
    }

    #[tokio::test]
    async fn body_stream_empty_with_only_trailers() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct TrailersOnlyBody {
            sent: bool,
        }

        impl http_body::Body for TrailersOnlyBody {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                if !self.sent {
                    self.sent = true;
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("x-trailer-key", "x-trailer-value".parse().unwrap());
                    Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let hyper_body: RequestBodySend = TrailersOnlyBody { sent: false }.boxed_unsync();
        let mut stream = BodyStreamSend::new(hyper_body);

        // No data frames — next() returns None without panicking
        assert!(stream.next().await.is_none());
    }

    #[cfg(feature = "compio")]
    #[test]
    fn body_stream_local_skips_trailers() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct TrailerThenData {
            state: u8,
        }

        impl http_body::Body for TrailerThenData {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                match self.state {
                    0 => {
                        self.state = 1;
                        let mut trailers = http::HeaderMap::new();
                        trailers.insert("x-local-test", "val".parse().unwrap());
                        Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
                    }
                    1 => {
                        self.state = 2;
                        Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from(
                            "after_trailer",
                        )))))
                    }
                    _ => Poll::Ready(None),
                }
            }
        }

        compio_runtime::Runtime::new().unwrap().block_on(async {
            let local_body: ResponseBodyLocal = Box::pin(TrailerThenData { state: 0 });
            let mut stream = BodyStreamLocal::with_observer(local_body, None);

            // The trailers frame should be skipped, only data returned
            let chunk = stream.next().await.unwrap().unwrap();
            assert_eq!(&chunk[..], b"after_trailer");

            // Then done
            assert!(stream.next().await.is_none());
        });
    }

    #[cfg(feature = "compio")]
    #[test]
    fn body_stream_local_skips_empty_data_frames() {
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct EmptyThenData {
            state: u8,
        }

        impl http_body::Body for EmptyThenData {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                let frame = match self.state {
                    0 => http_body::Frame::data(Bytes::new()),
                    1 => http_body::Frame::data(Bytes::from("hello")),
                    2 => http_body::Frame::data(Bytes::new()),
                    _ => return Poll::Ready(None),
                };
                self.state += 1;
                Poll::Ready(Some(Ok(frame)))
            }
        }

        compio_runtime::Runtime::new().unwrap().block_on(async {
            let local_body: ResponseBodyLocal = Box::pin(EmptyThenData { state: 0 });
            let mut stream = BodyStreamLocal::with_observer(local_body, None);

            let chunk = stream.next().await.unwrap().unwrap();
            assert_eq!(&chunk[..], b"hello");
            assert!(
                stream.next().await.is_none(),
                "empty data frames must not be yielded as empty chunks"
            );
        });
    }
}
