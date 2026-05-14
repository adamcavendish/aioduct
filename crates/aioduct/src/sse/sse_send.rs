use bytes::BytesMut;
use http_body_util::BodyExt;

use crate::body::RequestBoxBody;
use crate::error::Error;

use super::{SseDecoder, SseEvent};

/// Async iterator over a `text/event-stream` response body for `Send` runtimes.
pub struct SseStreamSend {
    body: RequestBoxBody,
    buf: BytesMut,
    decoder: SseDecoder,
    done: bool,
}

impl std::fmt::Debug for SseStreamSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseStreamSend").finish()
    }
}

impl SseStreamSend {
    pub(crate) fn new(body: RequestBoxBody) -> Self {
        Self {
            body,
            buf: BytesMut::new(),
            decoder: SseDecoder::new(),
            done: false,
        }
    }

    /// Create a stream with a custom maximum payload size per event.
    /// Pass `0` to disable the limit.
    pub fn with_max_payload_size(body: RequestBoxBody, max: usize) -> Self {
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
