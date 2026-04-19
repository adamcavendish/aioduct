use bytes::Bytes;
use http_body_util::BodyExt;

use crate::error::{HyperBody, Result};

/// HTTP request body, either buffered in memory or streaming.
pub enum RequestBody {
    /// Fully buffered body from bytes.
    Buffered(Bytes),
    /// Streaming body from a boxed hyper body.
    Streaming(HyperBody),
}

impl RequestBody {
    pub(crate) fn into_hyper_body(self) -> HyperBody {
        match self {
            RequestBody::Buffered(b) => http_body_util::Full::new(b)
                .map_err(|never| match never {})
                .boxed(),
            RequestBody::Streaming(body) => body,
        }
    }

    /// Clone this body if it is buffered. Returns `None` for streaming bodies.
    pub fn try_clone(&self) -> Option<Self> {
        match self {
            RequestBody::Buffered(b) => Some(RequestBody::Buffered(b.clone())),
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

impl From<HyperBody> for RequestBody {
    fn from(body: HyperBody) -> Self {
        RequestBody::Streaming(body)
    }
}

/// Async iterator over response body data frames.
pub struct BodyStream {
    body: HyperBody,
    done: bool,
}

impl BodyStream {
    pub(crate) fn new(body: HyperBody) -> Self {
        Self { body, done: false }
    }

    /// Returns the next chunk of body data, or `None` when complete.
    pub async fn next(&mut self) -> Option<Result<Bytes>> {
        if self.done {
            return None;
        }

        loop {
            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        return Some(Ok(data));
                    }
                }
                Some(Err(e)) => {
                    self.done = true;
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                    return None;
                }
            }
        }
    }
}
