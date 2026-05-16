#[cfg(not(target_arch = "wasm32"))]
use bytes::BytesMut;
#[cfg(not(target_arch = "wasm32"))]
use http_body_util::BodyExt;

#[cfg(not(target_arch = "wasm32"))]
use crate::error::Error;

#[cfg(not(target_arch = "wasm32"))]
use super::{SseDecoder, SseEvent};

/// `!Send` variant of [`SseStreamSend`](super::SseStreamSend) for completion-based runtimes.
#[cfg(not(target_arch = "wasm32"))]
pub struct SseStreamLocal {
    body: crate::body::ResponseBoxLocalBody,
    buf: BytesMut,
    decoder: SseDecoder,
    done: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for SseStreamLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseStreamLocal").finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SseStreamLocal {
    pub(crate) fn new(body: crate::body::ResponseBoxLocalBody) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            decoder: SseDecoder::new(),
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

#[cfg(all(test, not(target_arch = "wasm32"), feature = "tokio"))]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn sse_body(data: &[u8]) -> crate::body::ResponseBoxLocalBody {
        Box::pin(
            http_body_util::Full::new(bytes::Bytes::from(data.to_vec()))
                .map_err(|never| match never {}),
        )
    }

    #[tokio::test]
    async fn next_returns_single_event() {
        let body = sse_body(b"data: hello\n\n");
        let mut stream = SseStreamLocal::new(body);
        let event = stream.next().await.unwrap().unwrap();
        match event {
            SseEvent::Message(m) => assert_eq!(m.data, "hello"),
            _ => panic!("expected message"),
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_returns_multiple_events() {
        let body = sse_body(b"data: first\n\ndata: second\n\n");
        let mut stream = SseStreamLocal::new(body);
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
        let body = sse_body(b"");
        let mut stream = SseStreamLocal::new(body);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn next_with_event_type() {
        let body = sse_body(b"event: update\ndata: payload\n\n");
        let mut stream = SseStreamLocal::new(body);
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
        let body = sse_body(b"data: x\n\n");
        let mut stream = SseStreamLocal::new(body);
        let _ = stream.next().await;
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn debug_impl() {
        let body = sse_body(b"");
        let stream = SseStreamLocal::new(body);
        let dbg = format!("{stream:?}");
        assert!(dbg.contains("SseStreamLocal"));
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

        let body: crate::body::ResponseBoxLocalBody = Box::pin(ErrorBody);
        let mut stream = SseStreamLocal::new(body);
        let result = stream.next().await;
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }
}
