use bytes::Bytes;
use http_body_util::BodyExt;

use crate::error::{AioductBody, Error};

/// HTTP request body, either buffered in memory or streaming.
pub enum RequestBody {
    /// Fully buffered body from bytes.
    Buffered(Bytes),
    /// Streaming body from a boxed hyper body.
    Streaming(AioductBody),
}

impl std::fmt::Debug for RequestBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestBody::Buffered(_) => f.debug_tuple("Buffered").field(&"..").finish(),
            RequestBody::Streaming(_) => f.debug_tuple("Streaming").field(&"..").finish(),
        }
    }
}

impl RequestBody {
    pub(crate) fn into_hyper_body(self) -> AioductBody {
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

impl From<AioductBody> for RequestBody {
    fn from(body: AioductBody) -> Self {
        RequestBody::Streaming(body)
    }
}

/// Async iterator over response body data frames.
pub struct BodyStream {
    body: AioductBody,
    done: bool,
}

impl std::fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStream").finish()
    }
}

impl BodyStream {
    pub(crate) fn new(body: AioductBody) -> Self {
        Self { body, done: false }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn buffered(data: &[u8]) -> RequestBody {
        RequestBody::Buffered(Bytes::from(data.to_vec()))
    }

    fn streaming() -> RequestBody {
        let body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
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
        let hyper_body: AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let body: RequestBody = hyper_body.into();
        assert!(body.try_clone().is_none());
    }
}
