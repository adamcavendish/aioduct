use bytes::BytesMut;
use http_body::Body;
use http_body_util::BodyExt;

use crate::error::Error;

use super::{SseDecoder, SseEvent};

/// Async iterator over a `text/event-stream` response body.
///
/// Generic over the inner body type `B`. Obtained via
/// [`Response::into_sse_stream()`](crate::response::Response::into_sse_stream).
pub struct SseStream<B> {
    body: B,
    buf: BytesMut,
    decoder: SseDecoder,
    done: bool,
}

impl<B> std::fmt::Debug for SseStream<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseStream").finish()
    }
}

impl<B: Body<Data = bytes::Bytes, Error = Error> + Unpin> SseStream<B> {
    pub(crate) fn new(body: B) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            decoder: SseDecoder::new(),
            done: false,
        }
    }

    /// Create a stream with a custom maximum payload size per event.
    /// Pass `0` to disable the limit.
    pub fn with_max_payload_size(body: B, max: usize) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            decoder: SseDecoder::with_max_payload_size(max),
            done: false,
        }
    }

    /// Returns the next SSE event, or `None` when the stream ends.
    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
        loop {
            if let Some(event) = self.decoder.decode(&mut self.buf) {
                return Some(event);
            }

            if self.done {
                return None;
            }

            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.buf.extend_from_slice(&data);
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    self.done = true;
                    if let Some(event) = self.decoder.decode(&mut self.buf) {
                        return Some(event);
                    }
                    return None;
                }
            }
        }
    }
}

/// SSE stream for Send runtimes (tokio, smol).
pub type SseStreamSend = SseStream<crate::body::RequestBodySend>;

/// SSE stream for Local runtimes (compio).
#[cfg(not(target_arch = "wasm32"))]
pub type SseStreamLocal = SseStream<crate::body::ResponseBodyLocal>;

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn send_body(data: &[u8]) -> crate::body::RequestBodySend {
        http_body_util::Full::new(bytes::Bytes::from(data.to_vec()))
            .map_err(|never| match never {})
            .boxed_unsync()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn local_body(data: &[u8]) -> crate::body::ResponseBodyLocal {
        Box::pin(
            http_body_util::Full::new(bytes::Bytes::from(data.to_vec()))
                .map_err(|never| match never {}),
        )
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
        let mut stream = SseStream::with_max_payload_size(body, 1024);
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
        let mut stream: SseStreamLocal = SseStream::new(body);
        let event = stream.next().await.unwrap().unwrap();
        match event {
            SseEvent::Message(m) => assert_eq!(m.data, "local"),
            _ => panic!("expected message"),
        }
        assert!(stream.next().await.is_none());
    }
}
