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
