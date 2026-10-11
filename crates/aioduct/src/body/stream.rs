//! Async byte-stream iterators over response bodies.

use bytes::Bytes;
use http_body::Body;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::Error;

#[cfg(not(target_arch = "wasm32"))]
use super::ResponseBodyLocal;
use super::{BodyEnd, RequestBodySend, SendObservationHandle};
#[cfg(not(target_arch = "wasm32"))]
use crate::response::ResponseBodySend;

// ── BodyStreamSend ───────────────────────────────────────────────────────────

/// Async iterator over response body data frames.
pub struct BodyStreamSend {
    body: RequestBodySend,
    owner: Option<SendObservationHandle>,
    done: bool,
    /// Trailer headers captured from the body's trailer frame, if any.
    trailers: Option<http::HeaderMap>,
}

/// Future returned by [`BodyStreamSend::next`].
///
/// Usually used through `.next().await`; its representation is not a stable ABI.
#[must_use = "futures do nothing unless polled or awaited"]
pub struct BodyNextSend<'a> {
    stream: &'a mut BodyStreamSend,
}

impl Future for BodyNextSend<'_> {
    type Output = Option<Result<Bytes, Error>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().stream.poll_next(cx)
    }
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
            owner: None,
            done: false,
            trailers: None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn from_response_body(body: ResponseBodySend) -> Self {
        let (body, owner) = body.into_parts();
        Self {
            body,
            owner,
            done: false,
            trailers: None,
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
    #[allow(clippy::should_implement_trait)] // Async reading, not Iterator::next.
    pub fn next(&mut self) -> BodyNextSend<'_> {
        BodyNextSend { stream: self }
    }

    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, Error>>> {
        if self.done {
            return Poll::Ready(None);
        }

        loop {
            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(frame))) => {
                    if let Some(owner) = &self.owner
                        && self.body.is_end_stream()
                    {
                        owner.finish(BodyEnd::Complete);
                    }
                    match frame.into_data() {
                        Ok(data) => {
                            // Skip empty data frames: a consumer expects `Some` to
                            // carry non-empty data and `None` to mean end of stream.
                            if data.is_empty() {
                                continue;
                            }
                            return Poll::Ready(Some(Ok(data)));
                        }
                        Err(frame) => {
                            // Capture trailers so callers can read them via
                            // `trailers()` after the stream completes.
                            match frame.into_trailers() {
                                Ok(trailers) => match &mut self.trailers {
                                    Some(existing) => existing.extend(trailers),
                                    None => self.trailers = Some(trailers),
                                },
                                Err(_non_trailer) => {}
                            }
                        }
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    self.done = true;
                    if let Some(owner) = &self.owner {
                        owner.fail(&e);
                    }
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    self.done = true;
                    if let Some(owner) = &self.owner {
                        owner.finish(BodyEnd::Complete);
                    }
                    return Poll::Ready(None);
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BodyStreamSend {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            if let Some(owner) = &self.owner {
                owner.finish(BodyEnd::Abandoned);
            }
        }
    }
}

// ── BodyStreamLocal ──────────────────────────────────────────────────────────

/// Async iterator over response body data frames for `!Send` bodies.
///
/// Used by completion-based runtimes (compio) where the response body is not `Send`.
#[cfg(not(target_arch = "wasm32"))]
pub struct BodyStreamLocal {
    body: ResponseBodyLocal,
    done: bool,
    trailers: Option<http::HeaderMap>,
}

/// Future returned by [`BodyStreamLocal::next`].
///
/// Usually used through `.next().await`; its representation is not a stable ABI.
/// Like its stream, this future does not implement `Send`.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<aioduct::body::BodyNextLocal<'static>>();
/// ```
#[cfg(not(target_arch = "wasm32"))]
#[must_use = "futures do nothing unless polled or awaited"]
pub struct BodyNextLocal<'a> {
    stream: &'a mut BodyStreamLocal,
}

#[cfg(not(target_arch = "wasm32"))]
impl Future for BodyNextLocal<'_> {
    type Output = Option<Result<Bytes, Error>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().stream.poll_next(cx)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for BodyStreamLocal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStreamLocal").finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl BodyStreamLocal {
    pub(crate) fn from_response_body(body: ResponseBodyLocal) -> Self {
        Self {
            body,
            done: false,
            trailers: None,
        }
    }

    /// Returns the trailer headers received after the body, if any.
    ///
    /// Only populated once the stream has been fully consumed.
    pub fn trailers(&self) -> Option<&http::HeaderMap> {
        self.trailers.as_ref()
    }

    /// Returns the next chunk of body data, or `None` when complete.
    #[allow(clippy::should_implement_trait)] // Async reading, not Iterator::next.
    pub fn next(&mut self) -> BodyNextLocal<'_> {
        BodyNextLocal { stream: self }
    }

    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, Error>>> {
        if self.done {
            return Poll::Ready(None);
        }

        loop {
            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(data) => {
                        // Skip empty data frames: a consumer expects `Some` to
                        // carry non-empty data and `None` to mean end of stream.
                        if data.is_empty() {
                            continue;
                        }
                        return Poll::Ready(Some(Ok(data)));
                    }
                    Err(frame) => match frame.into_trailers() {
                        Ok(trailers) => match &mut self.trailers {
                            Some(existing) => existing.extend(trailers),
                            None => self.trailers = Some(trailers),
                        },
                        Err(_non_trailer) => {}
                    },
                },
                Poll::Ready(Some(Err(e))) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    self.done = true;
                    return Poll::Ready(None);
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BodyStreamLocal {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
        }
    }
}
