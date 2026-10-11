use bytes::BytesMut;
use http_body::Body;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::Error;

#[cfg(not(target_arch = "wasm32"))]
use super::observation::{LocalSseContinuation, SendSseContinuation};
use super::{SseDecoder, SseEvent};
use crate::observer::SseStatisticsSnapshot;

pub(super) trait SseSink {
    fn statistics(&mut self) -> Option<&mut SseStatisticsSnapshot> {
        None
    }
    fn observes_body(&self) -> bool {
        false
    }
    fn body_complete(&mut self) {}
    fn body_error(&mut self, _error: &Error) {}
    fn error(&mut self, _error: &Error) {}
    fn end(&mut self) {}
}

#[cfg(target_arch = "wasm32")]
struct Unobserved;
#[cfg(target_arch = "wasm32")]
impl SseSink for Unobserved {}

// Shared parsing/reading state; no public Observer type or lifecycle guard.
struct SseCore<B> {
    body: B,
    buf: BytesMut,
    decoder: SseDecoder,
    done: bool,
}

impl<B: Body<Data = bytes::Bytes, Error = Error> + Unpin> SseCore<B> {
    fn new(body: B) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            decoder: SseDecoder::new(),
            done: false,
        }
    }

    fn poll_next<S: SseSink>(
        &mut self,
        cx: &mut Context<'_>,
        sink: &mut S,
    ) -> Poll<Option<Result<SseEvent, Error>>> {
        loop {
            let event = if let Some(statistics) = sink.statistics() {
                let event = self.decoder.decode_observed(&mut self.buf, statistics);
                statistics.buffered_input_bytes = self.buf.len();
                if matches!(&event, Some(Ok(SseEvent::Message(_)))) {
                    statistics.messages_delivered = statistics.messages_delivered.saturating_add(1);
                }
                event
            } else {
                self.decoder.decode(&mut self.buf)
            };
            if let Some(event) = event {
                if let Err(error) = &event {
                    sink.error(error);
                }
                return Poll::Ready(Some(event));
            }
            if self.done {
                sink.end();
                return Poll::Ready(None);
            }
            match std::task::ready!(Pin::new(&mut self.body).poll_frame(cx)) {
                Some(Ok(frame)) => {
                    // A reliable last-frame fact belongs to HTTP/body, before
                    // SSE parsing/returning buffered messages.
                    if sink.observes_body() && self.body.is_end_stream() {
                        sink.body_complete();
                    }
                    if let Ok(data) = frame.into_data() {
                        self.buf.extend_from_slice(&data);
                        if let Some(statistics) = sink.statistics() {
                            statistics.buffered_input_bytes = self.buf.len();
                            statistics.peak_buffered_input_bytes =
                                statistics.peak_buffered_input_bytes.max(self.buf.len());
                        }
                    }
                }
                Some(Err(error)) => {
                    sink.body_error(&error);
                    sink.error(&error);
                    return Poll::Ready(Some(Err(error)));
                }
                None => {
                    sink.body_complete();
                    self.done = true;
                }
            }
        }
    }
}

/// Async iterator over a `text/event-stream` response body.
///
/// Generic over the inner body type `B`. Obtained via
/// [`Response::into_sse_stream()`](crate::response::Response::into_sse_stream).
pub struct SseStream<B> {
    core: SseCore<B>,
    #[cfg(not(target_arch = "wasm32"))]
    continuation: Option<Box<SendSseContinuation>>,
}

/// Future returned by [`SseStream::next`], normally consumed with `.await`.
/// Fields are private and its layout is not a stable ABI.
#[must_use = "futures do nothing unless polled or awaited"]
pub struct SseNext<'a, B> {
    stream: &'a mut SseStream<B>,
}

impl<B: Body<Data = bytes::Bytes, Error = Error> + Unpin> Future for SseNext<'_, B> {
    type Output = Option<Result<SseEvent, Error>>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let stream = &mut *self.get_mut().stream;
        #[cfg(not(target_arch = "wasm32"))]
        {
            stream.core.poll_next(cx, &mut stream.continuation)
        }
        #[cfg(target_arch = "wasm32")]
        {
            stream.core.poll_next(cx, &mut Unobserved)
        }
    }
}

impl<B> std::fmt::Debug for SseStream<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseStream").finish()
    }
}

impl<B: Body<Data = bytes::Bytes, Error = Error> + Unpin> SseStream<B> {
    pub(crate) fn new(body: B) -> Self {
        Self {
            core: SseCore::new(body),
            #[cfg(not(target_arch = "wasm32"))]
            continuation: None,
        }
    }

    /// Create a stream with a custom maximum payload size per event.
    /// Pass `0` to disable the limit.
    pub fn with_max_payload_size(mut self, max: usize) -> Self {
        self.core.decoder.set_max_payload_size(max);
        self
    }

    /// Returns the next SSE event, or `None` when the stream ends.
    #[allow(clippy::should_implement_trait)] // Async protocol reading, not Iterator.
    pub fn next(&mut self) -> SseNext<'_, B> {
        SseNext { stream: self }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SseStream<crate::response::ResponseBodySend> {
    pub(crate) fn from_response_body(mut body: crate::response::ResponseBodySend) -> Self {
        let owner = body.take_owner();
        let mut stream = Self::new(body);
        stream.continuation = owner.and_then(SendSseContinuation::from_owner);
        stream
    }
}

/// SSE stream for Send runtimes (tokio, smol).
///
/// The fixed response-body carrier keeps the private request continuation
/// attached through this conversion without exposing an observer type.
#[cfg(not(target_arch = "wasm32"))]
pub type SseStreamSend = SseStream<crate::response::ResponseBodySend>;

/// SSE stream for host-managed browser and WASI response bodies.
#[cfg(target_arch = "wasm32")]
pub type SseStreamSend = SseStream<crate::body::RequestBodySend>;

/// SSE stream for Local runtimes (compio).
#[cfg(not(target_arch = "wasm32"))]
pub struct SseStreamLocal {
    core: SseCore<crate::body::ResponseBodyLocal>,
    continuation: Option<Box<LocalSseContinuation>>,
}

/// Future returned by [`SseStreamLocal::next`]. It intentionally need not be Send.
#[cfg(not(target_arch = "wasm32"))]
#[must_use = "futures do nothing unless polled or awaited"]
pub struct SseNextLocal<'a> {
    stream: &'a mut SseStreamLocal,
}

#[cfg(not(target_arch = "wasm32"))]
impl Future for SseNextLocal<'_> {
    type Output = Option<Result<SseEvent, Error>>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let stream = &mut *self.get_mut().stream;
        stream.core.poll_next(cx, &mut stream.continuation)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for SseStreamLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseStreamLocal").finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SseStreamLocal {
    pub(crate) fn new(mut body: crate::body::ResponseBodyLocal) -> Self {
        let owner = body.take_owner();
        Self {
            core: SseCore::new(body),
            continuation: owner.and_then(LocalSseContinuation::from_owner),
        }
    }

    /// Set the maximum payload size for each event.
    /// Pass `0` to disable the limit.
    pub fn with_max_payload_size(mut self, max: usize) -> Self {
        self.core.decoder.set_max_payload_size(max);
        self
    }

    /// Returns the next SSE event, or `None` when the stream ends.
    #[allow(clippy::should_implement_trait)] // Async protocol reading, not Iterator.
    pub fn next(&mut self) -> SseNextLocal<'_> {
        SseNextLocal { stream: self }
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn selected_statistics_saturate_without_changing_protocol_output() {
        struct Statistics(SseStatisticsSnapshot);
        impl SseSink for Statistics {
            fn statistics(&mut self) -> Option<&mut SseStatisticsSnapshot> {
                Some(&mut self.0)
            }
        }
        let mut statistics = Statistics(SseStatisticsSnapshot {
            messages_delivered: u64::MAX,
            comment_lines_parsed: u64::MAX,
            retry_fields_accepted: u64::MAX,
            ..Default::default()
        });
        let mut core = SseCore::new(send_body(b":comment\nretry: 7\ndata: delivered\n\n"));
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            core.poll_next(&mut cx, &mut statistics),
            Poll::Ready(Some(Ok(SseEvent::Message(_))))
        ));
        assert_eq!(statistics.0.messages_delivered, u64::MAX);
        assert_eq!(statistics.0.comment_lines_parsed, u64::MAX);
        assert_eq!(statistics.0.retry_fields_accepted, u64::MAX);
    }

    fn send_body(data: &[u8]) -> crate::body::RequestBodySend {
        http_body_util::Full::new(bytes::Bytes::from(data.to_vec()))
            .map_err(|never| match never {})
            .boxed_unsync()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn local_body(data: &[u8]) -> crate::body::ResponseBodyLocal {
        crate::body::ResponseBodyLocal::from_inner(Box::pin(
            http_body_util::Full::new(bytes::Bytes::from(data.to_vec()))
                .map_err(|never| match never {}),
        ))
    }

    #[tokio::test]
    async fn next_returns_single_event() {
        let body = send_body(b"data: hello\n\n");
        let mut stream = SseStream::new(body);
        let event = stream.next().await.unwrap().unwrap();
        match event {
            SseEvent::Message(m) => assert_eq!(m.data, "hello"),
            _ => panic!("expected message"),
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_returns_multiple_events() {
        let body = send_body(b"data: first\n\ndata: second\n\n");
        let mut stream = SseStream::new(body);
        let e1 = stream.next().await.unwrap().unwrap();
        let e2 = stream.next().await.unwrap().unwrap();
        match (&e1, &e2) {
            (SseEvent::Message(m1), SseEvent::Message(m2)) => {
                assert_eq!(m1.data, "first");
                assert_eq!(m2.data, "second");
            }
            _ => panic!("expected two messages"),
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_returns_none_on_empty_body() {
        let body = send_body(b"");
        let mut stream = SseStream::new(body);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_with_event_type() {
        let body = send_body(b"event: update\ndata: payload\n\n");
        let mut stream = SseStream::new(body);
        let event = stream.next().await.unwrap().unwrap();
        match event {
            SseEvent::Message(m) => {
                assert_eq!(m.event, "update");
                assert_eq!(m.data, "payload");
            }
            _ => panic!("expected message"),
        }
    }

    #[tokio::test]
    async fn done_stays_none() {
        let body = send_body(b"data: x\n\n");
        let mut stream = SseStream::new(body);
        let _ = stream.next().await;
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn debug_impl() {
        let body = send_body(b"");
        let stream = SseStream::new(body);
        let dbg = format!("{stream:?}");
        assert!(dbg.contains("SseStream"));
    }

    #[tokio::test]
    async fn with_max_payload_size_works() {
        let body = send_body(b"data: short\n\n");
        let mut stream = SseStream::new(body).with_max_payload_size(1024);
        let event = stream.next().await.unwrap().unwrap();
        match event {
            SseEvent::Message(m) => assert_eq!(m.data, "short"),
            _ => panic!("expected message"),
        }
    }

    #[tokio::test]
    async fn next_propagates_body_error() {
        use bytes::Bytes;
        use http_body::Body;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct ErrorBody;

        impl Body for ErrorBody {
            type Data = Bytes;
            type Error = crate::error::Error;

            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                Poll::Ready(Some(Err(crate::error::Error::Other("stream error".into()))))
            }
        }

        let body: crate::body::RequestBodySend = http_body_util::BodyExt::boxed_unsync(ErrorBody);
        let mut stream = SseStream::new(body);
        let result = stream.next().await;
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn local_body_stream_works() {
        let body = local_body(b"data: local\n\n");
        let mut stream = SseStreamLocal::new(body);
        let event = stream.next().await.unwrap().unwrap();
        match event {
            SseEvent::Message(m) => assert_eq!(m.data, "local"),
            _ => panic!("expected message"),
        }
        assert!(stream.next().await.is_none());
    }
}
