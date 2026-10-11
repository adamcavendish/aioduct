use std::rc::Rc;
use std::sync::Arc;

use crate::body::{
    BodyEnd, LocalBodyObserver, LocalObservationHandle, SendBodyObserver, SendObservationHandle,
};
use crate::error::Error;
use crate::observer::{Interest, ResponseSseEvent, SseStatisticsSnapshot};

use super::stream::SseSink;

pub(super) trait Receiver {
    fn interest(&self) -> Interest;
    fn event(&self, event: ResponseSseEvent<'_>);
}

impl Receiver for Arc<dyn SendBodyObserver> {
    fn interest(&self) -> Interest {
        self.sse_interest()
    }
    fn event(&self, event: ResponseSseEvent<'_>) {
        self.on_response_sse(event);
    }
}

impl Receiver for Rc<dyn LocalBodyObserver> {
    fn interest(&self) -> Interest {
        self.sse_interest()
    }
    fn event(&self, event: ResponseSseEvent<'_>) {
        self.on_response_sse(event);
    }
}

pub(super) trait HttpOwner {
    type Receiver: Receiver;
    fn receiver(&self) -> Option<Self::Receiver>;
    fn is_terminal(&self) -> bool;
    fn complete(&self);
    fn fail(&self, error: &Error);
}

impl HttpOwner for SendObservationHandle {
    type Receiver = Arc<dyn SendBodyObserver>;
    fn receiver(&self) -> Option<Self::Receiver> {
        self.sse_receiver()
    }
    fn is_terminal(&self) -> bool {
        self.is_terminal()
    }
    fn complete(&self) {
        self.finish(BodyEnd::Complete);
    }
    fn fail(&self, error: &Error) {
        self.fail(error);
    }
}

impl HttpOwner for LocalObservationHandle {
    type Receiver = Rc<dyn LocalBodyObserver>;
    fn receiver(&self) -> Option<Self::Receiver> {
        self.sse_receiver()
    }
    fn is_terminal(&self) -> bool {
        self.is_terminal()
    }
    fn complete(&self) {
        self.finish(BodyEnd::Complete);
    }
    fn fail(&self, error: &Error) {
        self.fail(error);
    }
}

struct SseObservation<R: Receiver> {
    receiver: R,
    // Allocated only for the statistics interest, never stored inline on all streams.
    statistics: Option<Box<SseStatisticsSnapshot>>,
    active: bool,
}

impl<R: Receiver> SseObservation<R> {
    fn new(receiver: R) -> Self {
        let statistics = receiver
            .interest()
            .contains(Interest::SSE_STATISTICS)
            .then(|| Box::new(SseStatisticsSnapshot::default()));
        Self {
            receiver,
            statistics,
            active: true,
        }
    }
    fn end(&mut self) {
        if std::mem::replace(&mut self.active, false) {
            self.receiver.event(ResponseSseEvent::Ended {
                statistics: self.statistics.as_deref(),
            });
        }
    }
}

impl<R: Receiver> Drop for SseObservation<R> {
    fn drop(&mut self) {
        if std::mem::replace(&mut self.active, false) {
            self.receiver.event(ResponseSseEvent::Abandoned {
                statistics: self.statistics.as_deref(),
            });
        }
    }
}

/// Linear HTTP responsibility and independent SSE state share only the receiver.
/// This lives in the SSE wrapper, not in its inner response body carrier.
pub(super) struct SseContinuation<H: HttpOwner> {
    http: Option<H>,
    sse: Option<SseObservation<H::Receiver>>,
}

impl<H: HttpOwner> SseContinuation<H> {
    pub(super) fn from_owner(owner: H) -> Option<Box<Self>> {
        let sse = owner.receiver().map(SseObservation::new);
        let http = (!owner.is_terminal()).then_some(owner);
        if http.is_none() && sse.is_none() {
            return None;
        }
        let continuation = Box::new(Self { http, sse });
        if let Some(sse) = &continuation.sse {
            sse.receiver.event(ResponseSseEvent::Started);
        }
        Some(continuation)
    }
}

impl<H: HttpOwner> SseSink for Option<Box<SseContinuation<H>>> {
    fn statistics(&mut self) -> Option<&mut SseStatisticsSnapshot> {
        self.as_mut()?.sse.as_mut()?.statistics.as_deref_mut()
    }
    fn observes_body(&self) -> bool {
        self.as_ref()
            .is_some_and(|continuation| continuation.http.is_some())
    }
    fn body_complete(&mut self) {
        if let Some(continuation) = self
            && let Some(http) = continuation.http.take()
        {
            http.complete();
        }
    }
    fn body_error(&mut self, error: &Error) {
        if let Some(continuation) = self
            && let Some(http) = continuation.http.take()
        {
            http.fail(error);
        }
    }
    fn error(&mut self, error: &Error) {
        if let Some(sse) = self
            .as_ref()
            .and_then(|continuation| continuation.sse.as_ref())
        {
            sse.receiver.event(ResponseSseEvent::Error {
                error,
                statistics: sse.statistics.as_deref(),
            });
        }
    }
    fn end(&mut self) {
        if let Some(sse) = self
            .as_mut()
            .and_then(|continuation| continuation.sse.as_mut())
        {
            sse.end();
        }
        // Both actual lifecycles have ended. No subsequent next()/drop needs
        // the receiver or the selected statistics allocation.
        self.take();
    }
}

pub(super) type SendSseContinuation = SseContinuation<SendObservationHandle>;
pub(super) type LocalSseContinuation = SseContinuation<LocalObservationHandle>;

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
