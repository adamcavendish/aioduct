//! Observation state for host-managed Fetch/WASI clients.
//!
//! These clients can observe the request, selected response headers, and the
//! lifecycle of the body/SSE carrier. They cannot observe the host's DNS,
//! connection, negotiated protocol, or wire DATA boundaries, so those facts
//! are deliberately not synthesized here.

use super::{
    Instant, Interest, Observer, RequestFinish, RequestFinishReason, RequestInfo,
    RequestObservation, RequestStart, ResponseBodyEvent, ResponseHeaders, ResponseInfo,
    ResponseSource, ResponseSseEvent, SseStatisticsSnapshot,
};
use crate::error::Error;
use http::{Method, StatusCode, Uri};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub(crate) struct HostRequestObservation {
    inner: Rc<HostObservationInner>,
}

struct HostObservationInner {
    observation: RefCell<Option<Box<dyn RequestObservation>>>,
    interest: Interest,
    response: Cell<Option<ResponseInfo>>,
    attempt_count: Cell<u64>,
    body_finished: Cell<bool>,
    finished: Cell<bool>,
    sse_started: Cell<bool>,
    sse_finished: Cell<bool>,
    statistics: RefCell<Option<SseStatisticsSnapshot>>,
}

impl HostRequestObservation {
    pub(crate) fn begin<O>(
        observer: &O,
        method: &Method,
        uri: &Uri,
        context: Option<O::Context>,
    ) -> Option<Self>
    where
        O: Observer,
    {
        let (interest, observation) = observer.begin(
            RequestInfo {
                method,
                initial_uri: uri,
            },
            context,
        );
        let interest = interest.with_dependencies();
        if interest.is_empty() {
            return None;
        }

        let inner = Rc::new(HostObservationInner {
            observation: RefCell::new(Some(Box::new(observation))),
            interest,
            response: Cell::new(None),
            attempt_count: Cell::new(0),
            body_finished: Cell::new(false),
            finished: Cell::new(false),
            sse_started: Cell::new(false),
            sse_finished: Cell::new(false),
            statistics: RefCell::new(
                interest
                    .contains(Interest::SSE_STATISTICS)
                    .then(SseStatisticsSnapshot::default),
            ),
        });
        let this = Self { inner };
        if interest.contains(Interest::REQUEST_START) {
            this.with_observation(|observation| {
                observation.on_request_start(&RequestStart {
                    method,
                    initial_uri: uri,
                    at: Instant::now(),
                });
            });
        }
        Some(this)
    }

    /// Mark the one host-managed HTTP attempt immediately before invoking the
    /// host request API.  A request-builder failure before that point is not
    /// an attempted HTTP exchange.
    pub(crate) fn attempt_started(&self) {
        self.inner
            .attempt_count
            .set(self.inner.attempt_count.get().saturating_add(1));
    }

    pub(crate) fn observe_headers(&self, status: StatusCode) {
        if self.inner.response.get().is_some() {
            return;
        }
        let response = ResponseInfo {
            status,
            version: None,
            source: ResponseSource::Transport,
        };
        self.inner.response.set(Some(response));
        if self.inner.interest.contains(Interest::RESPONSE_HEADERS) {
            self.with_observation(|observation| {
                observation.on_response_headers(&ResponseHeaders {
                    response,
                    at: Instant::now(),
                });
            });
        }
    }

    pub(crate) fn execution_failed(&self, error: &Error) {
        if self.inner.response.get().is_some() {
            self.body_failed(error);
        } else {
            self.finish(RequestFinishReason::Failed { error });
        }
    }

    pub(crate) fn body_ended(&self) {
        if self.inner.body_finished.replace(true) {
            return;
        }
        if self.inner.interest.contains(Interest::RESPONSE_BODY) {
            self.with_observation(|observation| {
                observation.on_response_body(&ResponseBodyEvent::Ended { at: Instant::now() });
            });
        }
        self.finish(RequestFinishReason::Ended);
    }

    pub(crate) fn body_failed(&self, error: &Error) {
        if self.inner.body_finished.replace(true) {
            return;
        }
        if self.inner.interest.contains(Interest::RESPONSE_BODY) {
            self.with_observation(|observation| {
                observation.on_response_body(&ResponseBodyEvent::Failed {
                    error,
                    at: Instant::now(),
                });
            });
        }
        self.finish(RequestFinishReason::Failed { error });
    }

    pub(crate) fn body_abandoned(&self) {
        if self.inner.body_finished.replace(true) {
            return;
        }
        if self.inner.interest.contains(Interest::RESPONSE_BODY) {
            self.with_observation(|observation| {
                observation.on_response_body(&ResponseBodyEvent::Abandoned { at: Instant::now() });
            });
        }
        self.finish(RequestFinishReason::ConsumerAbandoned);
    }

    pub(crate) fn sse_started(&self) {
        if self.inner.sse_started.replace(true)
            || !self.inner.interest.contains(Interest::SSE_LIFECYCLE)
        {
            return;
        }
        self.with_observation(|observation| {
            observation.on_response_sse(ResponseSseEvent::Started);
        });
    }

    pub(crate) fn sse_error(&self, error: &Error) {
        if !self.inner.sse_started.get() || self.inner.sse_finished.get() {
            return;
        }
        let statistics = self.statistics_snapshot();
        if self.inner.interest.contains(Interest::SSE_LIFECYCLE) {
            self.with_observation(|observation| {
                observation.on_response_sse(ResponseSseEvent::Error {
                    error,
                    statistics: statistics.as_ref(),
                });
            });
        }
    }

    pub(crate) fn sse_failed(&self, error: &Error) {
        self.sse_error(error);
        self.inner.sse_finished.set(true);
        self.body_failed(error);
    }

    pub(crate) fn sse_event_delivered(&self, event: &crate::sse::SseEvent) {
        if !matches!(event, crate::sse::SseEvent::Message(_)) {
            return;
        }
        if let Some(statistics) = self.inner.statistics.borrow_mut().as_mut() {
            statistics.messages_delivered = statistics.messages_delivered.saturating_add(1);
        }
    }

    pub(crate) fn sse_buffered(&self, bytes: usize) {
        if let Some(statistics) = self.inner.statistics.borrow_mut().as_mut() {
            statistics.buffered_input_bytes = bytes;
            statistics.peak_buffered_input_bytes = statistics.peak_buffered_input_bytes.max(bytes);
        }
    }

    pub(crate) fn sse_ended(&self) {
        if !self.inner.sse_started.get() || self.inner.sse_finished.replace(true) {
            return;
        }
        let statistics = self.statistics_snapshot();
        if self.inner.interest.contains(Interest::SSE_LIFECYCLE) {
            self.with_observation(|observation| {
                observation.on_response_sse(ResponseSseEvent::Ended {
                    statistics: statistics.as_ref(),
                });
            });
        }
        self.body_ended();
    }

    pub(crate) fn sse_abandoned(&self) {
        if !self.inner.sse_started.get() || self.inner.sse_finished.replace(true) {
            return;
        }
        let statistics = self.statistics_snapshot();
        if self.inner.interest.contains(Interest::SSE_LIFECYCLE) {
            self.with_observation(|observation| {
                observation.on_response_sse(ResponseSseEvent::Abandoned {
                    statistics: statistics.as_ref(),
                });
            });
        }
        self.body_abandoned();
    }

    fn statistics_snapshot(&self) -> Option<SseStatisticsSnapshot> {
        self.inner.statistics.borrow().as_ref().copied()
    }

    fn finish(&self, reason: RequestFinishReason<'_>) {
        if self.inner.finished.replace(true) {
            return;
        }
        if self.inner.interest.contains(Interest::REQUEST_FINISH) {
            let response = self.inner.response.get();
            self.with_observation(|observation| {
                observation.on_request_finish(&RequestFinish {
                    reason,
                    response,
                    attempt_count: self.inner.attempt_count.get(),
                    original_data_bytes: None,
                    at: Instant::now(),
                });
            });
        }
    }

    fn with_observation(&self, callback: impl FnOnce(&dyn RequestObservation)) {
        let Some(observation) = self.inner.observation.borrow_mut().take() else {
            return;
        };
        callback(observation.as_ref());
        *self.inner.observation.borrow_mut() = Some(observation);
    }
}

impl Clone for HostRequestObservation {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl Drop for HostRequestObservation {
    fn drop(&mut self) {
        if Rc::strong_count(&self.inner) == 1 && !self.inner.finished.get() {
            if self.inner.response.get().is_some() {
                self.body_abandoned();
            } else {
                self.finish(RequestFinishReason::ExecutionAbandoned);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Records {
        headers: Cell<u32>,
        ended: Cell<u32>,
        body_failed: Cell<u32>,
        abandoned: Cell<u32>,
        execution_abandoned: Cell<u32>,
        attempt_count: Cell<u64>,
        unknown_version: Cell<bool>,
    }

    struct TestObservation(Rc<Records>);
    struct TestObserver(Rc<Records>);

    impl RequestObservation for TestObservation {
        fn on_response_headers(&self, event: &ResponseHeaders) {
            self.0.headers.set(self.0.headers.get() + 1);
            self.0.unknown_version.set(event.response.version.is_none());
        }

        fn on_response_body(&self, event: &ResponseBodyEvent<'_>) {
            if matches!(event, ResponseBodyEvent::Failed { .. }) {
                self.0.body_failed.set(self.0.body_failed.get() + 1);
            }
        }

        fn on_request_finish(&self, event: &RequestFinish<'_>) {
            match event.reason {
                RequestFinishReason::Ended => self.0.ended.set(self.0.ended.get() + 1),
                RequestFinishReason::ConsumerAbandoned => {
                    self.0.abandoned.set(self.0.abandoned.get() + 1)
                }
                RequestFinishReason::ExecutionAbandoned => self
                    .0
                    .execution_abandoned
                    .set(self.0.execution_abandoned.get() + 1),
                _ => {}
            }
            self.0.attempt_count.set(event.attempt_count);
        }
    }

    impl Observer for TestObserver {
        type Context = ();
        type Observation = TestObservation;
        type ConnectionObservation = super::super::NoopConnectionObservation;

        fn begin(
            &self,
            _: RequestInfo<'_>,
            _: Option<Self::Context>,
        ) -> (Interest, Self::Observation) {
            (
                Interest::RESPONSE_HEADERS | Interest::REQUEST_FINISH | Interest::RESPONSE_BODY,
                TestObservation(self.0.clone()),
            )
        }

        fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
            (Interest::EMPTY, super::super::NoopConnectionObservation)
        }
    }

    #[test]
    fn host_summary_reports_headers_and_unknown_version() {
        let records = Rc::new(Records {
            headers: Cell::new(0),
            ended: Cell::new(0),
            body_failed: Cell::new(0),
            abandoned: Cell::new(0),
            execution_abandoned: Cell::new(0),
            attempt_count: Cell::new(0),
            unknown_version: Cell::new(false),
        });
        let provider = TestObserver(records.clone());
        let method = Method::GET;
        let uri: Uri = "https://example.com/".parse().unwrap();
        let observation = HostRequestObservation::begin(&provider, &method, &uri, Some(()))
            .expect("interest is non-empty");
        observation.observe_headers(StatusCode::OK);
        observation.body_ended();
        assert_eq!(records.headers.get(), 1);
        assert_eq!(records.ended.get(), 1);
        assert!(records.unknown_version.get());
    }

    #[test]
    fn host_summary_reports_abandonment_without_extra_body_read() {
        let records = Rc::new(Records {
            headers: Cell::new(0),
            ended: Cell::new(0),
            body_failed: Cell::new(0),
            abandoned: Cell::new(0),
            execution_abandoned: Cell::new(0),
            attempt_count: Cell::new(0),
            unknown_version: Cell::new(false),
        });
        let provider = TestObserver(records.clone());
        let method = Method::GET;
        let uri: Uri = "https://example.com/".parse().unwrap();
        let observation = HostRequestObservation::begin(&provider, &method, &uri, None).unwrap();
        observation.observe_headers(StatusCode::OK);
        drop(observation);
        assert_eq!(records.abandoned.get(), 1);
        assert_eq!(records.ended.get(), 0);
    }

    #[test]
    fn host_drop_before_response_is_execution_abandoned() {
        let records = Rc::new(Records {
            headers: Cell::new(0),
            ended: Cell::new(0),
            body_failed: Cell::new(0),
            abandoned: Cell::new(0),
            execution_abandoned: Cell::new(0),
            attempt_count: Cell::new(0),
            unknown_version: Cell::new(false),
        });
        let provider = TestObserver(records.clone());
        let method = Method::GET;
        let uri: Uri = "https://example.com/".parse().unwrap();
        let observation = HostRequestObservation::begin(&provider, &method, &uri, None).unwrap();
        drop(observation);
        assert_eq!(records.execution_abandoned.get(), 1);
        assert_eq!(records.abandoned.get(), 0);
        assert_eq!(records.attempt_count.get(), 0);
    }

    #[test]
    fn host_finish_reports_only_started_attempts() {
        let records = Rc::new(Records {
            headers: Cell::new(0),
            ended: Cell::new(0),
            body_failed: Cell::new(0),
            abandoned: Cell::new(0),
            execution_abandoned: Cell::new(0),
            attempt_count: Cell::new(0),
            unknown_version: Cell::new(false),
        });
        let provider = TestObserver(records.clone());
        let method = Method::GET;
        let uri: Uri = "https://example.com/".parse().unwrap();
        let observation = HostRequestObservation::begin(&provider, &method, &uri, None).unwrap();
        observation.attempt_started();
        observation.observe_headers(StatusCode::OK);
        observation.body_ended();
        assert_eq!(records.attempt_count.get(), 1);
    }

    #[test]
    fn host_failure_after_headers_closes_body_lifecycle() {
        let records = Rc::new(Records {
            headers: Cell::new(0),
            ended: Cell::new(0),
            body_failed: Cell::new(0),
            abandoned: Cell::new(0),
            execution_abandoned: Cell::new(0),
            attempt_count: Cell::new(0),
            unknown_version: Cell::new(false),
        });
        let provider = TestObserver(records.clone());
        let method = Method::GET;
        let uri: Uri = "https://example.com/".parse().unwrap();
        let observation = HostRequestObservation::begin(&provider, &method, &uri, None).unwrap();
        observation.observe_headers(StatusCode::OK);
        let error = Error::Timeout;
        observation.execution_failed(&error);
        assert_eq!(records.body_failed.get(), 1);
        assert_eq!(records.abandoned.get(), 0);
    }

    #[test]
    fn host_body_terminal_is_reported_once() {
        let records = Rc::new(Records {
            headers: Cell::new(0),
            ended: Cell::new(0),
            body_failed: Cell::new(0),
            abandoned: Cell::new(0),
            execution_abandoned: Cell::new(0),
            attempt_count: Cell::new(0),
            unknown_version: Cell::new(false),
        });
        let provider = TestObserver(records.clone());
        let method = Method::GET;
        let uri: Uri = "https://example.com/".parse().unwrap();
        let observation = HostRequestObservation::begin(&provider, &method, &uri, None).unwrap();
        observation.body_ended();
        observation.body_ended();
        observation.body_abandoned();
        assert_eq!(records.ended.get(), 1);
        assert_eq!(records.abandoned.get(), 0);
    }
}
