//! HTTP request and response body types.
//!
//! This module provides the boxed body type aliases used throughout the
//! dispatch pipeline, the [`RequestBody`] enum (buffered vs streaming), and
//! the [`BodyStreamSend`] / [`BodyStreamLocal`] async byte-stream iterators.

mod stream;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod next_tests;

use bytes::Bytes;
use http_body_util::BodyExt;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(not(target_arch = "wasm32"))]
use std::cell::Cell;
#[cfg(not(target_arch = "wasm32"))]
use std::pin::Pin;
#[cfg(not(target_arch = "wasm32"))]
use std::rc::Rc;

use crate::error::Error;

#[cfg(not(target_arch = "wasm32"))]
pub use stream::{BodyNextLocal, BodyStreamLocal};
pub use stream::{BodyNextSend, BodyStreamSend};

/// The terminal fact observed when a response body continuation ends.
///
/// This is an internal carrier contract. The public observer maps these facts
/// to the request-level finish reason; response and stream types do not expose
/// the observer or its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyEnd {
    Complete,
    Error,
    Abandoned,
    HandedOff,
}

/// Receives terminal facts for a Send response body continuation.
pub(crate) trait SendBodyObserver: Send + Sync + 'static {
    fn on_body_end(&self, end: BodyEnd);

    fn on_response_body(&self, _event: &crate::observer::ResponseBodyEvent<'_>) {}

    fn on_body_error(&self, _error: &Error) {
        self.on_body_end(BodyEnd::Error);
    }

    fn sse_interest(&self) -> crate::observer::Interest {
        crate::observer::Interest::EMPTY
    }

    fn on_response_sse(&self, _event: crate::observer::ResponseSseEvent<'_>) {}

    fn on_response_channel_handoff(
        &self,
        _event: &crate::observer::ResponseChannelHandoffEvent<'_>,
    ) {
    }

    fn on_channel_io(&self, _event: &crate::observer::ChannelIoEvent<'_>) {}
}

/// Private, linear Send-side continuation owner.
///
/// Callback references may be shared, but terminal responsibility is moved,
/// never cloned. The owner is only stored inside aioduct carriers.
pub(crate) struct SendObservationHandle {
    observer: Arc<dyn SendBodyObserver>,
    terminal: AtomicBool,
    awaiting_handoff: bool,
}

impl SendObservationHandle {
    pub(crate) fn channel_receiver(&self) -> Option<Arc<dyn SendBodyObserver>> {
        self.observer
            .sse_interest()
            .intersects(
                crate::observer::Interest::RESPONSE_CHANNEL_HANDOFF
                    | crate::observer::Interest::CHANNEL_LIFECYCLE,
            )
            .then(|| self.observer.clone())
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    pub(crate) fn sse_receiver(&self) -> Option<Arc<dyn SendBodyObserver>> {
        self.observer
            .sse_interest()
            .contains(crate::observer::Interest::SSE_LIFECYCLE)
            .then(|| self.observer.clone())
    }
    #[allow(dead_code)]
    pub(crate) fn new(observer: Arc<dyn SendBodyObserver>) -> Self {
        Self {
            observer,
            terminal: AtomicBool::new(false),
            awaiting_handoff: false,
        }
    }

    pub(crate) fn finish(&self, end: BodyEnd) {
        if end == BodyEnd::Complete && self.awaiting_handoff {
            return;
        }
        if !self.terminal.swap(true, Ordering::AcqRel) {
            self.observer.on_body_end(end);
        }
    }

    pub(crate) fn handoff(&self) {
        self.finish(BodyEnd::HandedOff);
    }

    pub(crate) fn await_handoff(&mut self) {
        self.awaiting_handoff = true;
    }

    pub(crate) fn fail(&self, error: &Error) {
        if !self.terminal.swap(true, Ordering::AcqRel) {
            self.observer.on_body_error(error);
        }
    }

    /// Drop an execution-side owner without reporting a response-body fact.
    ///
    /// This is used when the request fails or is cancelled before a response
    /// was handed to the caller. Such an owner is not a consumer-owned body,
    /// so its drop must not look like consumer abandonment.
    pub(crate) fn discard(&self) {
        self.terminal.store(true, Ordering::Release);
    }
}

impl Drop for SendObservationHandle {
    fn drop(&mut self) {
        self.finish(BodyEnd::Abandoned);
    }
}

// ── Boxed body type aliases ──────────────────────────────────────────────────

/// Boxed request body (always `Send`).
///
/// Used as the body type for `http::Request<RequestBodySend>` throughout the
/// dispatch pipeline. Based on `UnsyncBoxBody` from `http-body-util`.
pub type RequestBodySend = http_body_util::combinators::UnsyncBoxBody<Bytes, Error>;

#[cfg(not(target_arch = "wasm32"))]
pin_project_lite::pin_project! {
    /// Counts source bytes for the low-frequency request-body observation.
    /// It deliberately reports only source end or source error, never frames
    /// or Pending polls.
    pub(crate) struct ObservedRequestBodySend {
        #[pin]
        inner: RequestBodySend,
        observer: crate::observer::SendAttemptObserver,
        attempt: u64,
        bytes: u64,
        ended: bool,
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ObservedRequestBodySend {
    pub(crate) fn new(
        inner: RequestBodySend,
        observer: crate::observer::SendAttemptObserver,
    ) -> Self {
        let attempt = observer.current_attempt();
        Self {
            inner,
            observer,
            attempt,
            bytes: 0,
            ended: false,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl http_body::Body for ObservedRequestBodySend {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    *this.bytes = this.bytes.saturating_add(data.len() as u64);
                }
                if this.inner.is_end_stream() && !*this.ended {
                    *this.ended = true;
                    this.observer.request_body_ended(*this.attempt, *this.bytes);
                }
                std::task::Poll::Ready(Some(Ok(frame)))
            }
            std::task::Poll::Ready(None) => {
                if !*this.ended {
                    *this.ended = true;
                    this.observer.request_body_ended(*this.attempt, *this.bytes);
                }
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Ready(Some(Err(error))) => {
                if !*this.ended {
                    *this.ended = true;
                    this.observer.request_body_failed(*this.attempt, &error);
                }
                std::task::Poll::Ready(Some(Err(error)))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn observe_request_body_send(
    body: RequestBodySend,
    observer: crate::observer::SendAttemptObserver,
) -> RequestBodySend {
    http_body_util::BodyExt::boxed_unsync(ObservedRequestBodySend::new(body, observer))
}

/// Boxed `!Send` request body for completion-based runtimes (compio).
///
/// Used in the Local execution path where the body may contain `!Send` state
/// (e.g. compio futures). The body is created, polled, and dropped on the
/// same thread — no cross-thread migration occurs.
#[cfg(not(target_arch = "wasm32"))]
pub type RequestBodyLocal = Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + 'static>>;

#[cfg(not(target_arch = "wasm32"))]
pin_project_lite::pin_project! {
    /// Local counterpart of [`ObservedRequestBodySend`].
    pub(crate) struct ObservedRequestBodyLocal {
        #[pin]
        inner: RequestBodyLocal,
        observer: crate::observer::LocalAttemptObserver,
        attempt: u64,
        bytes: u64,
        ended: bool,
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ObservedRequestBodyLocal {
    pub(crate) fn new(
        inner: RequestBodyLocal,
        observer: crate::observer::LocalAttemptObserver,
    ) -> Self {
        let attempt = observer.current_attempt();
        Self {
            inner,
            observer,
            attempt,
            bytes: 0,
            ended: false,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl http_body::Body for ObservedRequestBodyLocal {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    *this.bytes = this.bytes.saturating_add(data.len() as u64);
                }
                if this.inner.is_end_stream() && !*this.ended {
                    *this.ended = true;
                    this.observer.request_body_ended(*this.attempt, *this.bytes);
                }
                std::task::Poll::Ready(Some(Ok(frame)))
            }
            std::task::Poll::Ready(None) => {
                if !*this.ended {
                    *this.ended = true;
                    this.observer.request_body_ended(*this.attempt, *this.bytes);
                }
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Ready(Some(Err(error))) => {
                if !*this.ended {
                    *this.ended = true;
                    this.observer.request_body_failed(*this.attempt, &error);
                }
                std::task::Poll::Ready(Some(Err(error)))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn observe_request_body_local(
    body: RequestBodyLocal,
    observer: crate::observer::LocalAttemptObserver,
) -> RequestBodyLocal {
    Box::pin(ObservedRequestBodyLocal::new(body, observer))
}

/// `!Send` response body carrier for completion-based runtimes (compio).
///
/// This is intentionally a nominal type rather than a type alias. The inner
/// body is erased, while the private owner slot gives the response pipeline a
/// linear place to carry request-scoped state across raw-body, SSE, and
/// upgrade hand-offs without exposing an observer type parameter to callers.
#[cfg(not(target_arch = "wasm32"))]
pub struct ResponseBodyLocal {
    inner: RequestBodyLocal,
    owner: Option<LocalObservationHandle>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type LocalBodyEnd = BodyEnd;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) trait LocalBodyObserver: 'static {
    fn on_body_end(&self, end: LocalBodyEnd);

    fn on_response_body(&self, _event: &crate::observer::ResponseBodyEvent<'_>) {}

    fn on_body_error(&self, _error: &Error) {
        self.on_body_end(LocalBodyEnd::Error);
    }

    fn sse_interest(&self) -> crate::observer::Interest {
        crate::observer::Interest::EMPTY
    }

    fn on_response_sse(&self, _event: crate::observer::ResponseSseEvent<'_>) {}

    fn on_response_channel_handoff(
        &self,
        _event: &crate::observer::ResponseChannelHandoffEvent<'_>,
    ) {
    }

    fn on_channel_io(&self, _event: &crate::observer::ChannelIoEvent<'_>) {}
}

#[cfg(not(target_arch = "wasm32"))]
impl LocalBodyObserver for SendObservationHandle {
    fn on_response_body(&self, event: &crate::observer::ResponseBodyEvent<'_>) {
        self.observer.on_response_body(event);
    }

    fn on_response_channel_handoff(
        &self,
        event: &crate::observer::ResponseChannelHandoffEvent<'_>,
    ) {
        self.observer.on_response_channel_handoff(event);
    }

    fn on_channel_io(&self, event: &crate::observer::ChannelIoEvent<'_>) {
        self.observer.on_channel_io(event);
    }

    fn sse_interest(&self) -> crate::observer::Interest {
        self.observer.sse_interest()
    }

    fn on_response_sse(&self, event: crate::observer::ResponseSseEvent<'_>) {
        self.observer.on_response_sse(event);
    }

    fn on_body_end(&self, end: LocalBodyEnd) {
        self.finish(end);
    }

    fn on_body_error(&self, error: &Error) {
        self.fail(error);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
struct NoopLocalBodyObserver;

#[cfg(not(target_arch = "wasm32"))]
impl LocalBodyObserver for NoopLocalBodyObserver {
    fn on_body_end(&self, _end: LocalBodyEnd) {}
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct LocalObservationHandle {
    observer: Rc<dyn LocalBodyObserver>,
    terminal: Cell<bool>,
    awaiting_handoff: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl LocalObservationHandle {
    pub(crate) fn channel_receiver(&self) -> Option<Rc<dyn LocalBodyObserver>> {
        self.observer
            .sse_interest()
            .intersects(
                crate::observer::Interest::RESPONSE_CHANNEL_HANDOFF
                    | crate::observer::Interest::CHANNEL_LIFECYCLE,
            )
            .then(|| self.observer.clone())
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal.get()
    }

    pub(crate) fn sse_receiver(&self) -> Option<Rc<dyn LocalBodyObserver>> {
        self.observer
            .sse_interest()
            .contains(crate::observer::Interest::SSE_LIFECYCLE)
            .then(|| self.observer.clone())
    }

    pub(crate) fn from_send(owner: SendObservationHandle) -> Self {
        Self::new(Rc::new(owner))
    }
    #[allow(dead_code)]
    pub(crate) fn new(observer: Rc<dyn LocalBodyObserver>) -> Self {
        Self {
            observer,
            terminal: Cell::new(false),
            awaiting_handoff: false,
        }
    }

    pub(crate) fn finish(&self, end: LocalBodyEnd) {
        if end == BodyEnd::Complete && self.awaiting_handoff {
            return;
        }
        if !self.terminal.replace(true) {
            self.observer.on_body_end(end);
        }
    }

    pub(crate) fn handoff(&self) {
        self.finish(LocalBodyEnd::HandedOff);
    }

    pub(crate) fn await_handoff(&mut self) {
        self.awaiting_handoff = true;
    }

    pub(crate) fn fail(&self, error: &Error) {
        if !self.terminal.replace(true) {
            self.observer.on_body_error(error);
        }
    }

    /// See [`SendObservationHandle::discard`].
    pub(crate) fn discard(&self) {
        self.terminal.set(true);
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn new_for_test() -> Self {
        Self::new(Rc::new(NoopLocalBodyObserver))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for LocalObservationHandle {
    fn drop(&mut self) {
        self.finish(LocalBodyEnd::Abandoned);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ResponseBodyLocal {
    pub(crate) fn channel_receiver(&self) -> Option<Rc<dyn LocalBodyObserver>> {
        self.owner.as_ref()?.channel_receiver()
    }

    pub(crate) fn take_owner(&mut self) -> Option<LocalObservationHandle> {
        self.owner.take()
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_inner(
        inner: Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + 'static>>,
    ) -> Self {
        Self { inner, owner: None }
    }

    pub(crate) fn with_owner(
        inner: Pin<Box<dyn http_body::Body<Data = Bytes, Error = Error> + 'static>>,
        owner: Option<LocalObservationHandle>,
    ) -> Self {
        Self { inner, owner }
    }

    pub(crate) fn into_parts(self) -> (RequestBodyLocal, Option<LocalObservationHandle>) {
        (self.inner, self.owner)
    }

    pub(crate) fn finish_handoff(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.handoff();
        }
    }

    pub(crate) fn finish_error(&self, error: &Error) {
        if let Some(owner) = &self.owner {
            owner.fail(error);
        }
    }

    pub(crate) fn set_owner(&mut self, owner: LocalObservationHandle) {
        debug_assert!(self.owner.is_none(), "response body owner was already set");
        self.owner = Some(owner);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl http_body::Body for ResponseBodyLocal {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let result = self.inner.as_mut().poll_frame(cx);
        match &result {
            std::task::Poll::Ready(Some(Err(error))) => {
                if let Some(owner) = &self.owner {
                    owner.fail(error);
                }
            }
            std::task::Poll::Ready(None) => {
                if let Some(owner) = &self.owner {
                    owner.finish(LocalBodyEnd::Complete);
                }
            }
            std::task::Poll::Ready(Some(Ok(_))) => {
                if let Some(owner) = &self.owner
                    && self.inner.is_end_stream()
                {
                    owner.finish(LocalBodyEnd::Complete);
                }
            }
            std::task::Poll::Pending => {}
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

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
