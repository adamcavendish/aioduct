use super::{
    AttemptEvent, Instant, Interest, Observer, RequestFinish, RequestFinishReason, RequestInfo,
    RequestObservation, RequestStart, ResponseDisposition, ResponseHeaders, ResponseInfo,
    RetryReason, original_data,
};
use http::{Method, Uri};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The Send-side request continuation kept by the execution pipeline.
///
/// This is deliberately erased before it reaches a response body.  The
/// public response type therefore remains independent of the configured
/// observer and of its associated observation type.
pub(crate) struct SendRequestObservationHandle {
    inner: Arc<SendObservationInner>,
    owner: Option<crate::body::SendObservationHandle>,
}

#[derive(Clone, Default)]
pub(crate) struct RequestAttemptCounter {
    total: Arc<AtomicU64>,
    interest: Interest,
}

pub(crate) trait AttemptStartObserver {
    fn start_time(&self) -> Option<Instant>;
    fn started(&self, attempt: u64, method: &Method, uri: &Uri, at: Instant);
}

#[derive(Clone, Copy, Default)]
pub(crate) struct NoopAttemptStartObserver;

impl AttemptStartObserver for NoopAttemptStartObserver {
    fn start_time(&self) -> Option<Instant> {
        None
    }

    fn started(&self, _attempt: u64, _method: &Method, _uri: &Uri, _at: Instant) {}
}

impl RequestAttemptCounter {
    fn new(interest: Interest) -> Self {
        Self {
            total: Arc::new(AtomicU64::new(0)),
            interest,
        }
    }
    pub(crate) fn interest(&self) -> Interest {
        self.interest
    }
    pub(crate) fn started(&self) -> u64 {
        let mut current = self.total.load(Ordering::Relaxed);
        loop {
            if current == u64::MAX {
                return current;
            }
            match self.total.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return current + 1,
                Err(observed) => current = observed,
            }
        }
    }
    fn value(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub(crate) fn current(&self) -> u64 {
        self.value()
    }
}

struct SendObservationInner {
    observation: Box<dyn RequestObservation + Send + Sync>,
    interest: Interest,
    response: Mutex<Option<ResponseInfo>>,
    attempt_count: Option<RequestAttemptCounter>,
    original_data: Mutex<Option<Arc<AtomicU64>>>,
    finished: AtomicBool,
    failed_attempt: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct SendAttemptObserver(Arc<SendObservationInner>);

pub(crate) trait AttemptDiagnostics {
    fn emit(&self, event: &AttemptEvent<'_>);
    fn acquisition(&self, event: &super::ConnectionAcquisitionEvent<'_>);
    fn acquisition_scope(
        &self,
        phase: super::ConnectionAcquisitionPhase,
    ) -> Option<AcquisitionPhaseScope<'_, Self>>
    where
        Self: Sized;
}

/// Sequential establishment phases owned outside a timed child future.
/// Dropping that child on timeout must not turn a real error into cancellation.
pub(crate) struct AcquisitionPhases<'a, T: AttemptDiagnostics> {
    observer: Option<&'a T>,
    active: Option<AcquisitionPhaseScope<'a, T>>,
}

impl<'a, T: AttemptDiagnostics> AcquisitionPhases<'a, T> {
    pub(crate) fn new(observer: Option<&'a T>) -> Self {
        Self {
            observer,
            active: None,
        }
    }

    pub(crate) fn start(&mut self, phase: super::ConnectionAcquisitionPhase) {
        debug_assert!(
            self.active.is_none(),
            "previous acquisition phase is still active"
        );
        self.active = self
            .observer
            .and_then(|observer| observer.acquisition_scope(phase));
    }

    pub(crate) fn complete(&mut self) {
        if let Some(mut scope) = self.active.take() {
            scope.complete();
        }
    }

    pub(crate) fn finish(&mut self, error: Option<&crate::Error>) {
        if let Some(mut scope) = self.active.take() {
            if let Some(error) = error {
                scope.failed(error);
            } else {
                scope.complete();
            }
        }
    }
}

pub(crate) struct AcquisitionPhaseScope<'a, T: AttemptDiagnostics> {
    observer: &'a T,
    phase: super::ConnectionAcquisitionPhase,
    attempt: u64,
    completed: bool,
}

impl<T: AttemptDiagnostics> AcquisitionPhaseScope<'_, T> {
    pub(crate) fn complete(&mut self) {
        if !self.completed {
            self.completed = true;
            self.observer
                .acquisition(&super::ConnectionAcquisitionEvent::PhaseCompleted {
                    attempt: self.attempt,
                    phase: self.phase,
                    at: Instant::now(),
                });
        }
    }

    pub(crate) fn result<U>(&mut self, result: &Result<U, crate::Error>) {
        match result {
            Ok(_) => self.complete(),
            Err(error) => self.failed(error),
        }
    }

    pub(crate) fn failed(&mut self, error: &crate::Error) {
        if !self.completed {
            self.completed = true;
            self.observer
                .acquisition(&super::ConnectionAcquisitionEvent::Failed {
                    attempt: self.attempt,
                    phase: self.phase,
                    error,
                    at: Instant::now(),
                });
        }
    }
}

impl<T: AttemptDiagnostics> Drop for AcquisitionPhaseScope<'_, T> {
    fn drop(&mut self) {
        if !self.completed {
            self.completed = true;
            self.observer
                .acquisition(&super::ConnectionAcquisitionEvent::Cancelled {
                    attempt: self.attempt,
                    phase: self.phase,
                    at: Instant::now(),
                });
        }
    }
}

pub(crate) struct AttemptBackoff<'a, T: AttemptDiagnostics> {
    observer: &'a T,
    attempt: u64,
    delay: std::time::Duration,
    completed: bool,
}

pub(crate) struct RateLimitAdmission<'a, T: AttemptDiagnostics> {
    observer: &'a T,
    attempt: u64,
    admitted: bool,
}

impl<T: AttemptDiagnostics> RateLimitAdmission<'_, T> {
    pub(crate) fn admitted(mut self) {
        self.admitted = true;
        self.observer.emit(&AttemptEvent::RateLimit {
            attempt: self.attempt,
            state: super::RateLimitState::Admitted,
            at: Instant::now(),
        });
    }
}

impl<T: AttemptDiagnostics> Drop for RateLimitAdmission<'_, T> {
    fn drop(&mut self) {
        if !self.admitted {
            self.admitted = true;
            self.observer.emit(&AttemptEvent::RateLimit {
                attempt: self.attempt,
                state: super::RateLimitState::Cancelled,
                at: Instant::now(),
            });
        }
    }
}

impl<T: AttemptDiagnostics> AttemptBackoff<'_, T> {
    pub(crate) fn complete(mut self) {
        self.completed = true;
        self.observer.emit(&AttemptEvent::Backoff {
            attempt: self.attempt,
            delay: self.delay,
            state: super::BackoffState::Completed,
            at: Instant::now(),
        });
    }
}

impl<T: AttemptDiagnostics> Drop for AttemptBackoff<'_, T> {
    fn drop(&mut self) {
        if !self.completed {
            self.completed = true;
            self.observer.emit(&AttemptEvent::Backoff {
                attempt: self.attempt,
                delay: self.delay,
                state: super::BackoffState::Cancelled,
                at: Instant::now(),
            });
        }
    }
}

pub(crate) struct AttemptCleanup<'a, T: AttemptDiagnostics> {
    observer: &'a T,
    attempt: u64,
    completed: bool,
}

impl<T: AttemptDiagnostics> AttemptCleanup<'_, T> {
    pub(crate) fn finish(mut self, state: super::CleanupState<'_>, bytes: Option<u64>) {
        self.completed = true;
        self.observer.emit(&AttemptEvent::Cleanup {
            attempt: self.attempt,
            state,
            bytes,
            at: Instant::now(),
        });
    }

    pub(crate) fn drained(self, result: &Result<bytes::Bytes, crate::error::Error>) {
        match result {
            Ok(bytes) => self.finish(super::CleanupState::Drained, Some(bytes.len() as u64)),
            Err(error) => self.finish(super::CleanupState::Failed { error }, None),
        }
    }
}

impl<T: AttemptDiagnostics> Drop for AttemptCleanup<'_, T> {
    fn drop(&mut self) {
        if !self.completed {
            self.completed = true;
            self.observer.emit(&AttemptEvent::Cleanup {
                attempt: self.attempt,
                state: super::CleanupState::Stopped,
                bytes: None,
                at: Instant::now(),
            });
        }
    }
}

macro_rules! attempt_diagnostics {
    ($ty:ty) => {
        impl AttemptDiagnostics for $ty {
            fn emit(&self, event: &AttemptEvent<'_>) {
                self.0.on_attempt(event);
            }
            fn acquisition(&self, event: &super::ConnectionAcquisitionEvent<'_>) {
                self.0.observation.on_connection_acquisition(event);
            }
            fn acquisition_scope(
                &self,
                phase: super::ConnectionAcquisitionPhase,
            ) -> Option<AcquisitionPhaseScope<'_, Self>> {
                self.acquisition_scope(phase)
            }
        }

        impl $ty {
            pub(crate) fn rate_limit(&self) -> Option<RateLimitAdmission<'_, Self>> {
                if !self.0.interest.contains(Interest::ATTEMPT) {
                    return None;
                }
                let attempt = self.current_attempt();
                self.emit(&AttemptEvent::RateLimit {
                    attempt,
                    state: super::RateLimitState::Started,
                    at: Instant::now(),
                });
                Some(RateLimitAdmission {
                    observer: self,
                    attempt,
                    admitted: false,
                })
            }
            pub(crate) fn acquisition_scope(
                &self,
                phase: super::ConnectionAcquisitionPhase,
            ) -> Option<AcquisitionPhaseScope<'_, Self>> {
                if !self.acquisition_enabled() {
                    return None;
                }
                self.acquisition_started(phase);
                Some(AcquisitionPhaseScope {
                    observer: self,
                    phase,
                    attempt: self.current_attempt(),
                    completed: false,
                })
            }
            pub(crate) fn backoff(
                &self,
                delay: std::time::Duration,
            ) -> Option<AttemptBackoff<'_, Self>> {
                if !self.0.interest.contains(Interest::ATTEMPT) {
                    return None;
                }
                let attempt = self.current_attempt();
                self.emit(&AttemptEvent::Backoff {
                    attempt,
                    delay,
                    state: super::BackoffState::Started,
                    at: Instant::now(),
                });
                Some(AttemptBackoff {
                    observer: self,
                    attempt,
                    delay,
                    completed: false,
                })
            }

            pub(crate) fn cleanup(&self, attempt: u64) -> Option<AttemptCleanup<'_, Self>> {
                if !self.0.interest.contains(Interest::ATTEMPT) {
                    return None;
                }
                self.emit(&AttemptEvent::CleanupStarted {
                    attempt,
                    at: Instant::now(),
                });
                Some(AttemptCleanup {
                    observer: self,
                    attempt,
                    completed: false,
                })
            }
        }
    };
}

attempt_diagnostics!(SendAttemptObserver);
attempt_diagnostics!(LocalAttemptObserver);

impl SendAttemptObserver {
    pub(crate) fn request_body_enabled(&self) -> bool {
        self.0.interest.contains(Interest::REQUEST_BODY)
    }

    pub(crate) fn acquisition_enabled(&self) -> bool {
        self.0.interest.contains(Interest::CONNECTION_ACQUISITION)
    }

    pub(crate) fn started(&self, attempt: u64, method: &Method, uri: &Uri) {
        if !self.0.interest.contains(Interest::ATTEMPT) {
            return;
        }
        self.0.on_attempt(&AttemptEvent::Started {
            attempt,
            method,
            uri,
            at: Instant::now(),
        });
    }

    pub(crate) fn response_disposition(
        &self,
        attempt: u64,
        response: ResponseInfo,
        disposition: ResponseDisposition,
    ) {
        if !self.0.interest.contains(Interest::ATTEMPT) {
            return;
        }
        self.0.on_attempt(&AttemptEvent::ResponseDisposition {
            attempt,
            disposition,
            response,
            at: Instant::now(),
        });
    }

    pub(crate) fn retry_scheduled(&self, attempt: u64, next_attempt: u64, reason: RetryReason<'_>) {
        if !self.0.interest.contains(Interest::ATTEMPT) {
            return;
        }
        self.0.on_attempt(&AttemptEvent::RetryScheduled {
            attempt,
            next_attempt,
            reason,
            at: Instant::now(),
        });
    }

    pub(crate) fn failed(&self, attempt: u64, error: &crate::error::Error) {
        if !self.0.interest.contains(Interest::ATTEMPT)
            || attempt == 0
            || self.0.failed_attempt.fetch_max(attempt, Ordering::Relaxed) >= attempt
        {
            return;
        }
        self.0.on_attempt(&AttemptEvent::Failed {
            attempt,
            error,
            at: Instant::now(),
        });
    }

    pub(crate) fn request_body_ended(&self, attempt: u64, bytes: u64) {
        if self.0.interest.contains(Interest::REQUEST_BODY) {
            self.0
                .observation
                .on_request_body(&super::RequestBodyEvent::Ended {
                    attempt,
                    bytes: Some(bytes),
                    at: Instant::now(),
                });
        }
    }

    pub(crate) fn request_body_failed(&self, attempt: u64, error: &crate::error::Error) {
        if self.0.interest.contains(Interest::REQUEST_BODY) {
            self.0
                .observation
                .on_request_body(&super::RequestBodyEvent::Failed {
                    attempt,
                    error,
                    at: Instant::now(),
                });
        }
    }

    pub(crate) fn stale_retry(&self, error: &crate::error::Error) {
        if self.0.interest.contains(Interest::ATTEMPT) {
            let attempt = self.current_attempt();
            self.failed(attempt, error);
            self.retry_scheduled(
                attempt,
                attempt.saturating_add(1),
                RetryReason::StaleConnection,
            );
        }
    }

    pub(crate) fn current_attempt(&self) -> u64 {
        self.0
            .attempt_count
            .as_ref()
            .map_or(0, RequestAttemptCounter::current)
    }

    pub(crate) fn observe_headers(&self, response: ResponseInfo) {
        self.0.observe_headers(response);
    }

    pub(crate) fn acquisition_started(&self, phase: super::ConnectionAcquisitionPhase) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PhaseStarted {
                    attempt: self.current_attempt(),
                    phase,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn pool_lookup(&self, result: super::PoolLookupResult) {
        if self.acquisition_enabled() {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PoolLookup {
                    attempt: self.current_attempt(),
                    result,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_completed(&self, phase: super::ConnectionAcquisitionPhase) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PhaseCompleted {
                    attempt: self.current_attempt(),
                    phase,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_failed(
        &self,
        phase: super::ConnectionAcquisitionPhase,
        error: &crate::error::Error,
    ) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::Failed {
                    attempt: self.current_attempt(),
                    phase,
                    error,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_cancelled(&self, phase: super::ConnectionAcquisitionPhase) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::Cancelled {
                    attempt: self.current_attempt(),
                    phase,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_skipped(
        &self,
        phase: super::ConnectionAcquisitionPhase,
        reason: super::PhaseSkipReason,
    ) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PhaseSkipped {
                    attempt: self.current_attempt(),
                    phase,
                    reason,
                },
            );
        }
    }

    pub(crate) fn acquisition_acquired(
        &self,
        resource: super::ConnectionAcquisitionResource,
        connection: super::ConnectionAssociation,
        version: http::Version,
    ) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::ConnectionAcquired {
                    attempt: self.current_attempt(),
                    resource,
                    connection,
                    version,
                    at: Instant::now(),
                },
            );
        }
    }
}

#[derive(Clone)]
pub(crate) struct LocalAttemptObserver(std::rc::Rc<LocalObservationInner>);

impl AttemptStartObserver for LocalAttemptObserver {
    fn start_time(&self) -> Option<Instant> {
        self.0
            .interest
            .contains(Interest::ATTEMPT)
            .then(Instant::now)
    }

    fn started(&self, attempt: u64, method: &Method, uri: &Uri, at: Instant) {
        if !self.0.interest.contains(Interest::ATTEMPT) {
            return;
        }
        self.0.on_attempt(&AttemptEvent::Started {
            attempt,
            method,
            uri,
            at,
        });
    }
}

impl LocalAttemptObserver {
    pub(crate) fn request_body_enabled(&self) -> bool {
        self.0.interest.contains(Interest::REQUEST_BODY)
    }

    pub(crate) fn acquisition_enabled(&self) -> bool {
        self.0.interest.contains(Interest::CONNECTION_ACQUISITION)
    }

    pub(crate) fn response_disposition(
        &self,
        attempt: u64,
        response: ResponseInfo,
        disposition: ResponseDisposition,
    ) {
        if !self.0.interest.contains(Interest::ATTEMPT) {
            return;
        }
        self.0.on_attempt(&AttemptEvent::ResponseDisposition {
            attempt,
            disposition,
            response,
            at: Instant::now(),
        });
    }

    pub(crate) fn retry_scheduled(&self, attempt: u64, next_attempt: u64, reason: RetryReason<'_>) {
        if !self.0.interest.contains(Interest::ATTEMPT) {
            return;
        }
        self.0.on_attempt(&AttemptEvent::RetryScheduled {
            attempt,
            next_attempt,
            reason,
            at: Instant::now(),
        });
    }

    pub(crate) fn failed(&self, attempt: u64, error: &crate::error::Error) {
        if !self.0.interest.contains(Interest::ATTEMPT)
            || attempt == 0
            || self.0.failed_attempt.fetch_max(attempt, Ordering::Relaxed) >= attempt
        {
            return;
        }
        self.0.on_attempt(&AttemptEvent::Failed {
            attempt,
            error,
            at: Instant::now(),
        });
    }

    pub(crate) fn request_body_ended(&self, attempt: u64, bytes: u64) {
        if self.0.interest.contains(Interest::REQUEST_BODY) {
            self.0
                .observation
                .on_request_body(&super::RequestBodyEvent::Ended {
                    attempt,
                    bytes: Some(bytes),
                    at: Instant::now(),
                });
        }
    }

    pub(crate) fn request_body_failed(&self, attempt: u64, error: &crate::error::Error) {
        if self.0.interest.contains(Interest::REQUEST_BODY) {
            self.0
                .observation
                .on_request_body(&super::RequestBodyEvent::Failed {
                    attempt,
                    error,
                    at: Instant::now(),
                });
        }
    }

    pub(crate) fn stale_retry(&self, error: &crate::error::Error) {
        if self.0.interest.contains(Interest::ATTEMPT) {
            let attempt = self.current_attempt();
            self.failed(attempt, error);
            self.retry_scheduled(
                attempt,
                attempt.saturating_add(1),
                RetryReason::StaleConnection,
            );
        }
    }

    pub(crate) fn current_attempt(&self) -> u64 {
        self.0
            .attempt_count
            .as_ref()
            .map_or(0, RequestAttemptCounter::current)
    }

    pub(crate) fn observe_headers(&self, response: ResponseInfo) {
        self.0.observe_headers(response);
    }

    pub(crate) fn acquisition_started(&self, phase: super::ConnectionAcquisitionPhase) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PhaseStarted {
                    attempt: self.current_attempt(),
                    phase,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn pool_lookup(&self, result: super::PoolLookupResult) {
        if self.acquisition_enabled() {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PoolLookup {
                    attempt: self.current_attempt(),
                    result,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_completed(&self, phase: super::ConnectionAcquisitionPhase) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PhaseCompleted {
                    attempt: self.current_attempt(),
                    phase,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_failed(
        &self,
        phase: super::ConnectionAcquisitionPhase,
        error: &crate::error::Error,
    ) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::Failed {
                    attempt: self.current_attempt(),
                    phase,
                    error,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_cancelled(&self, phase: super::ConnectionAcquisitionPhase) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::Cancelled {
                    attempt: self.current_attempt(),
                    phase,
                    at: Instant::now(),
                },
            );
        }
    }

    pub(crate) fn acquisition_skipped(
        &self,
        phase: super::ConnectionAcquisitionPhase,
        reason: super::PhaseSkipReason,
    ) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::PhaseSkipped {
                    attempt: self.current_attempt(),
                    phase,
                    reason,
                },
            );
        }
    }

    pub(crate) fn acquisition_acquired(
        &self,
        resource: super::ConnectionAcquisitionResource,
        connection: super::ConnectionAssociation,
        version: http::Version,
    ) {
        if self.0.interest.contains(Interest::CONNECTION_ACQUISITION) {
            self.0.observation.on_connection_acquisition(
                &super::ConnectionAcquisitionEvent::ConnectionAcquired {
                    attempt: self.current_attempt(),
                    resource,
                    connection,
                    version,
                    at: Instant::now(),
                },
            );
        }
    }
}

impl AttemptStartObserver for Option<LocalAttemptObserver> {
    fn start_time(&self) -> Option<Instant> {
        self.as_ref().and_then(AttemptStartObserver::start_time)
    }

    fn started(&self, attempt: u64, method: &Method, uri: &Uri, at: Instant) {
        if let Some(observer) = self {
            observer.started(attempt, method, uri, at);
        }
    }
}

impl SendObservationInner {
    fn on_attempt(&self, event: &AttemptEvent<'_>) {
        if self.interest.contains(Interest::ATTEMPT) {
            self.observation.on_attempt(event);
        }
    }

    fn has_response(&self) -> bool {
        self.response
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    fn claim_finish(&self) -> bool {
        !self.finished.swap(true, Ordering::AcqRel)
    }

    fn emit_finish(&self, reason: RequestFinishReason<'_>) {
        if !self.interest.contains(Interest::REQUEST_FINISH) {
            return;
        }
        let response = *self
            .response
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let original_data_bytes = self
            .original_data
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|bytes| bytes.load(Ordering::Relaxed));
        self.observation.on_request_finish(&RequestFinish {
            reason,
            response,
            attempt_count: self
                .attempt_count
                .as_ref()
                .map_or(0, RequestAttemptCounter::value),
            original_data_bytes,
            at: Instant::now(),
        });
    }

    fn finish(&self, reason: RequestFinishReason<'_>) {
        if !self.claim_finish() {
            return;
        }
        self.emit_finish(reason);
    }

    fn observe_headers(&self, response: ResponseInfo) {
        if self.interest.contains(Interest::RESPONSE_HEADERS) {
            *self
                .response
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(response);
            self.observation.on_response_headers(&ResponseHeaders {
                response,
                at: Instant::now(),
            });
        } else {
            *self
                .response
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(response);
        }
    }
}

impl crate::body::SendBodyObserver for SendObservationInner {
    fn on_response_body(&self, event: &super::ResponseBodyEvent<'_>) {
        if self.interest.contains(Interest::RESPONSE_BODY) {
            self.observation.on_response_body(event);
        }
    }

    fn on_response_channel_handoff(&self, event: &super::ResponseChannelHandoffEvent<'_>) {
        self.observation.on_response_channel_handoff(event);
    }
    fn on_channel_io(&self, event: &super::ChannelIoEvent<'_>) {
        self.observation.on_channel_io(event);
    }
    fn sse_interest(&self) -> Interest {
        self.interest
    }

    fn on_response_sse(&self, event: super::ResponseSseEvent<'_>) {
        self.observation.on_response_sse(event);
    }
    fn on_body_end(&self, end: crate::body::BodyEnd) {
        // The execution-side owner also exists before response selection. Its
        // drop is not a caller abandoning a returned response body.
        if !self.has_response() || !self.claim_finish() {
            return;
        }
        let event = match end {
            crate::body::BodyEnd::Complete => {
                super::ResponseBodyEvent::Ended { at: Instant::now() }
            }
            crate::body::BodyEnd::Abandoned => {
                super::ResponseBodyEvent::Abandoned { at: Instant::now() }
            }
            crate::body::BodyEnd::HandedOff => {
                super::ResponseBodyEvent::HandedOff { at: Instant::now() }
            }
            crate::body::BodyEnd::Error => return,
        };
        self.on_response_body(&event);
        match end {
            crate::body::BodyEnd::Complete => self.emit_finish(RequestFinishReason::Ended),
            crate::body::BodyEnd::Abandoned => {
                self.emit_finish(RequestFinishReason::ConsumerAbandoned)
            }
            crate::body::BodyEnd::HandedOff => self.emit_finish(RequestFinishReason::HandedOff),
            crate::body::BodyEnd::Error => {
                unreachable!("body errors must be delivered with their original Error")
            }
        }
    }

    fn on_body_error(&self, error: &crate::error::Error) {
        if !self.has_response() || !self.claim_finish() {
            return;
        }
        let event = super::ResponseBodyEvent::Failed {
            error,
            at: Instant::now(),
        };
        self.on_response_body(&event);
        self.emit_finish(RequestFinishReason::Failed { error });
    }
}

impl SendRequestObservationHandle {
    pub(crate) fn new<O>(
        observer: &O,
        request: RequestInfo<'_>,
        context: Option<O::Context>,
    ) -> Option<Self>
    where
        O: Observer,
        O::Context: Send,
        O::Observation: Send + Sync,
    {
        let (interest, observation) = observer.begin(request, context);
        let interest = interest.with_dependencies();
        if interest.is_empty() {
            return None;
        }
        let inner = Arc::new(SendObservationInner {
            observation: Box::new(observation),
            interest,
            response: Mutex::new(None),
            attempt_count: interest
                .intersects(
                    Interest::REQUEST_FINISH
                        | Interest::ATTEMPT
                        | Interest::CONNECTION_ACQUISITION
                        | Interest::REQUEST_BODY
                        | Interest::RESPONSE_CHANNEL_HANDOFF
                        | Interest::CHANNEL_LIFECYCLE,
                )
                .then(|| RequestAttemptCounter::new(interest)),
            original_data: Mutex::new(None),
            finished: AtomicBool::new(false),
            failed_attempt: AtomicU64::new(0),
        });
        if interest.contains(Interest::REQUEST_START) {
            inner.observation.on_request_start(&RequestStart {
                method: request.method,
                initial_uri: request.initial_uri,
                at: Instant::now(),
            });
        }
        let owner = interest
            .intersects(
                Interest::RESPONSE_BODY
                    .union(Interest::REQUEST_FINISH)
                    .union(Interest::SSE_LIFECYCLE)
                    .union(Interest::RESPONSE_CHANNEL_HANDOFF)
                    .union(Interest::CHANNEL_LIFECYCLE),
            )
            .then(|| crate::body::SendObservationHandle::new(inner.clone()));
        Some(Self { inner, owner })
    }

    pub(crate) fn attempt_counter(&self) -> Option<RequestAttemptCounter> {
        self.inner.attempt_count.clone()
    }

    pub(crate) fn attempt_observer(&self) -> Option<SendAttemptObserver> {
        self.inner
            .interest
            .intersects(
                Interest::ATTEMPT | Interest::CONNECTION_ACQUISITION | Interest::REQUEST_BODY,
            )
            .then(|| SendAttemptObserver(self.inner.clone()))
    }

    pub(crate) fn response_observer(&self) -> SendAttemptObserver {
        SendAttemptObserver(self.inner.clone())
    }

    pub(crate) fn observe_headers(&self, response: ResponseInfo) {
        self.inner.observe_headers(response);
    }

    pub(crate) fn count_original_data(
        &self,
        response: crate::response::Response,
    ) -> crate::response::Response {
        if !self.inner.interest.contains(Interest::RESPONSE_BODY_BYTES) {
            return response;
        }
        let snapshot = Arc::new(AtomicU64::new(0));
        *self
            .inner
            .original_data
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(snapshot.clone());
        response.map_body(|body| {
            use http_body_util::BodyExt;
            let (body, owner) = body.into_parts();
            crate::response::ResponseBodySend::with_owner(
                original_data::OriginalDataBody::new(body, snapshot).boxed_unsync(),
                owner,
            )
        })
    }

    pub(crate) fn finish_failed(&mut self, error: &crate::error::Error) {
        if self.inner.has_response() {
            if let Some(owner) = self.owner.take() {
                owner.fail(error);
            } else {
                self.inner.finish(RequestFinishReason::Failed { error });
            }
        } else {
            if let Some(owner) = self.owner.take() {
                owner.discard();
            }
            self.inner.finish(RequestFinishReason::Failed { error });
        }
    }

    pub(crate) fn finish_ended(&mut self) {
        if let Some(owner) = &self.owner {
            owner.finish(crate::body::BodyEnd::Complete);
        } else {
            self.inner.finish(RequestFinishReason::Ended);
        }
        if !self.inner.interest.contains(Interest::SSE_LIFECYCLE) {
            self.owner.take();
        }
    }

    pub(crate) fn attach_to_send_response(
        &mut self,
        response: &mut crate::response::Response,
        awaiting_handoff: bool,
    ) {
        if let Some(mut owner) = self.owner.take() {
            if awaiting_handoff {
                owner.await_handoff();
            } else if http_body::Body::is_end_stream(response.body_mut()) {
                owner.finish(crate::body::BodyEnd::Complete);
            }
            response.body_mut().set_owner(owner);
        }
    }
}

impl Drop for SendRequestObservationHandle {
    fn drop(&mut self) {
        if self.owner.is_some() {
            if let Some(owner) = &self.owner {
                owner.discard();
            }
            self.inner.finish(RequestFinishReason::ExecutionAbandoned);
        }
    }
}

pub(crate) struct LocalRequestObservationHandle {
    inner: std::rc::Rc<LocalObservationInner>,
    owner: Option<crate::body::LocalObservationHandle>,
}

struct LocalObservationInner {
    observation: Box<dyn RequestObservation>,
    interest: Interest,
    response: std::cell::RefCell<Option<ResponseInfo>>,
    attempt_count: Option<RequestAttemptCounter>,
    original_data: std::cell::RefCell<Option<Arc<AtomicU64>>>,
    finished: std::cell::Cell<bool>,
    failed_attempt: AtomicU64,
}

impl LocalObservationInner {
    fn on_attempt(&self, event: &AttemptEvent<'_>) {
        if self.interest.contains(Interest::ATTEMPT) {
            self.observation.on_attempt(event);
        }
    }

    fn has_response(&self) -> bool {
        self.response.borrow().is_some()
    }

    fn claim_finish(&self) -> bool {
        !self.finished.replace(true)
    }

    fn emit_finish(&self, reason: RequestFinishReason<'_>) {
        if !self.interest.contains(Interest::REQUEST_FINISH) {
            return;
        }
        let response = *self.response.borrow();
        let original_data_bytes = self
            .original_data
            .borrow()
            .as_ref()
            .map(|bytes| bytes.load(Ordering::Relaxed));
        self.observation.on_request_finish(&RequestFinish {
            reason,
            response,
            attempt_count: self
                .attempt_count
                .as_ref()
                .map_or(0, RequestAttemptCounter::value),
            original_data_bytes,
            at: Instant::now(),
        });
    }

    fn finish(&self, reason: RequestFinishReason<'_>) {
        if !self.claim_finish() {
            return;
        }
        self.emit_finish(reason);
    }

    fn observe_headers(&self, response: ResponseInfo) {
        *self.response.borrow_mut() = Some(response);
        if self.interest.contains(Interest::RESPONSE_HEADERS) {
            self.observation.on_response_headers(&ResponseHeaders {
                response,
                at: Instant::now(),
            });
        }
    }
}

impl crate::body::LocalBodyObserver for LocalObservationInner {
    fn on_response_body(&self, event: &super::ResponseBodyEvent<'_>) {
        if self.interest.contains(Interest::RESPONSE_BODY) {
            self.observation.on_response_body(event);
        }
    }

    fn on_response_channel_handoff(&self, event: &super::ResponseChannelHandoffEvent<'_>) {
        self.observation.on_response_channel_handoff(event);
    }
    fn on_channel_io(&self, event: &super::ChannelIoEvent<'_>) {
        self.observation.on_channel_io(event);
    }
    fn sse_interest(&self) -> Interest {
        self.interest
    }

    fn on_response_sse(&self, event: super::ResponseSseEvent<'_>) {
        self.observation.on_response_sse(event);
    }
    fn on_body_end(&self, end: crate::body::BodyEnd) {
        if !self.has_response() || !self.claim_finish() {
            return;
        }
        let event = match end {
            crate::body::BodyEnd::Complete => {
                super::ResponseBodyEvent::Ended { at: Instant::now() }
            }
            crate::body::BodyEnd::Abandoned => {
                super::ResponseBodyEvent::Abandoned { at: Instant::now() }
            }
            crate::body::BodyEnd::HandedOff => {
                super::ResponseBodyEvent::HandedOff { at: Instant::now() }
            }
            crate::body::BodyEnd::Error => return,
        };
        self.on_response_body(&event);
        match end {
            crate::body::BodyEnd::Complete => self.emit_finish(RequestFinishReason::Ended),
            crate::body::BodyEnd::Abandoned => {
                self.emit_finish(RequestFinishReason::ConsumerAbandoned)
            }
            crate::body::BodyEnd::HandedOff => self.emit_finish(RequestFinishReason::HandedOff),
            crate::body::BodyEnd::Error => {
                unreachable!("body errors must be delivered with their original Error")
            }
        }
    }

    fn on_body_error(&self, error: &crate::error::Error) {
        if !self.has_response() || !self.claim_finish() {
            return;
        }
        let event = super::ResponseBodyEvent::Failed {
            error,
            at: Instant::now(),
        };
        self.on_response_body(&event);
        self.emit_finish(RequestFinishReason::Failed { error });
    }
}

impl LocalRequestObservationHandle {
    pub(crate) fn new<O>(
        observer: &O,
        request: RequestInfo<'_>,
        context: Option<O::Context>,
    ) -> Option<Self>
    where
        O: Observer,
    {
        let (interest, observation) = observer.begin(request, context);
        let interest = interest.with_dependencies();
        if interest.is_empty() {
            return None;
        }
        let inner = std::rc::Rc::new(LocalObservationInner {
            observation: Box::new(observation),
            interest,
            response: std::cell::RefCell::new(None),
            attempt_count: interest
                .intersects(
                    Interest::REQUEST_FINISH
                        | Interest::ATTEMPT
                        | Interest::CONNECTION_ACQUISITION
                        | Interest::REQUEST_BODY
                        | Interest::RESPONSE_CHANNEL_HANDOFF
                        | Interest::CHANNEL_LIFECYCLE,
                )
                .then(|| RequestAttemptCounter::new(interest)),
            original_data: std::cell::RefCell::new(None),
            finished: std::cell::Cell::new(false),
            failed_attempt: AtomicU64::new(0),
        });
        if interest.contains(Interest::REQUEST_START) {
            inner.observation.on_request_start(&RequestStart {
                method: request.method,
                initial_uri: request.initial_uri,
                at: Instant::now(),
            });
        }
        let owner = interest
            .intersects(
                Interest::RESPONSE_BODY
                    .union(Interest::REQUEST_FINISH)
                    .union(Interest::SSE_LIFECYCLE)
                    .union(Interest::RESPONSE_CHANNEL_HANDOFF)
                    .union(Interest::CHANNEL_LIFECYCLE),
            )
            .then(|| crate::body::LocalObservationHandle::new(inner.clone()));
        Some(Self { inner, owner })
    }

    pub(crate) fn attempt_counter(&self) -> Option<RequestAttemptCounter> {
        self.inner.attempt_count.clone()
    }

    pub(crate) fn attempt_observer(&self) -> Option<LocalAttemptObserver> {
        self.inner
            .interest
            .intersects(
                Interest::ATTEMPT | Interest::CONNECTION_ACQUISITION | Interest::REQUEST_BODY,
            )
            .then(|| LocalAttemptObserver(self.inner.clone()))
    }

    pub(crate) fn response_observer(&self) -> LocalAttemptObserver {
        LocalAttemptObserver(self.inner.clone())
    }

    pub(crate) fn observe_headers(&self, response: ResponseInfo) {
        self.inner.observe_headers(response);
    }

    pub(crate) fn count_original_data(
        &self,
        response: crate::response::Response,
    ) -> crate::response::Response {
        if !self.inner.interest.contains(Interest::RESPONSE_BODY_BYTES) {
            return response;
        }
        let snapshot = Arc::new(AtomicU64::new(0));
        *self.inner.original_data.borrow_mut() = Some(snapshot.clone());
        response.map_body(|body| {
            use http_body_util::BodyExt;
            let (body, owner) = body.into_parts();
            crate::response::ResponseBodySend::with_owner(
                original_data::OriginalDataBody::new(body, snapshot).boxed_unsync(),
                owner,
            )
        })
    }

    pub(crate) fn finish_failed(&mut self, error: &crate::error::Error) {
        if self.inner.has_response() {
            if let Some(owner) = self.owner.take() {
                owner.fail(error);
            } else {
                self.inner.finish(RequestFinishReason::Failed { error });
            }
        } else {
            if let Some(owner) = self.owner.take() {
                owner.discard();
            }
            self.inner.finish(RequestFinishReason::Failed { error });
        }
    }

    pub(crate) fn finish_ended(&mut self) {
        if let Some(owner) = &self.owner {
            owner.finish(crate::body::BodyEnd::Complete);
        } else {
            self.inner.finish(RequestFinishReason::Ended);
        }
        if !self.inner.interest.contains(Interest::SSE_LIFECYCLE) {
            self.owner.take();
        }
    }

    pub(crate) fn attach_to_local_response(
        &mut self,
        response: &mut crate::response::Response<crate::body::ResponseBodyLocal>,
        awaiting_handoff: bool,
    ) {
        if let Some(mut owner) = self.owner.take() {
            if awaiting_handoff {
                owner.await_handoff();
            } else if http_body::Body::is_end_stream(response.body_mut()) {
                owner.finish(crate::body::BodyEnd::Complete);
            }
            response.body_mut().set_owner(owner);
        }
    }
}

impl Drop for LocalRequestObservationHandle {
    fn drop(&mut self) {
        if self.owner.is_some() {
            if let Some(owner) = &self.owner {
                owner.discard();
            }
            self.inner.finish(RequestFinishReason::ExecutionAbandoned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::observer::NoopConnectionObservation;
    use bytes::Bytes;
    use http_body_util::BodyExt;

    #[derive(Clone, Default)]
    struct Phases(Arc<Mutex<Vec<(u64, super::super::ConnectionAcquisitionPhase)>>>);

    impl Observer for Phases {
        type Context = ();
        type Observation = Self;
        type ConnectionObservation = NoopConnectionObservation;
        fn begin_connection(&self) -> (Interest, NoopConnectionObservation) {
            (Interest::EMPTY, NoopConnectionObservation)
        }
        fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self) {
            (Interest::CONNECTION_ACQUISITION, self.clone())
        }
    }
    impl RequestObservation for Phases {
        fn on_connection_acquisition(&self, event: &super::super::ConnectionAcquisitionEvent<'_>) {
            if let super::super::ConnectionAcquisitionEvent::PhaseCompleted {
                attempt, phase, ..
            } = event
            {
                self.0.lock().unwrap().push((*attempt, *phase));
            }
        }
    }

    macro_rules! late_acquisition {
        ($name:ident, $handle:ty) => {
            #[test]
            fn $name() {
                let observer = Phases::default();
                let method = Method::GET;
                let uri = "http://localhost/".parse().unwrap();
                let handle = <$handle>::new(
                    &observer,
                    RequestInfo {
                        method: &method,
                        initial_uri: &uri,
                    },
                    None,
                )
                .unwrap();
                let counter = handle.attempt_counter().unwrap();
                assert_eq!(counter.started(), 1);
                let attempt = handle.attempt_observer().unwrap();
                let phase = super::super::ConnectionAcquisitionPhase::TlsHandshake;
                let mut scope = attempt.acquisition_scope(phase).unwrap();
                assert_eq!(counter.started(), 2);
                scope.complete();
                drop(scope);
                assert_eq!(*observer.0.lock().unwrap(), vec![(1, phase)]);
            }
        };
    }
    late_acquisition!(
        send_late_acquisition_retains_owning_attempt,
        SendRequestObservationHandle
    );
    late_acquisition!(
        local_late_acquisition_retains_owning_attempt,
        LocalRequestObservationHandle
    );

    #[derive(Debug)]
    struct Marker(Arc<()>);

    impl std::fmt::Display for Marker {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("original source")
        }
    }
    impl std::error::Error for Marker {}

    struct Recorder {
        source: Arc<()>,
        errors: Arc<AtomicU64>,
    }
    impl Observer for Recorder {
        type Context = ();
        type Observation = Self;
        type ConnectionObservation = NoopConnectionObservation;
        fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
            (Interest::EMPTY, NoopConnectionObservation)
        }
        fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self) {
            (
                Interest::REQUEST_FINISH,
                Self {
                    source: self.source.clone(),
                    errors: self.errors.clone(),
                },
            )
        }
    }
    impl RequestObservation for Recorder {
        fn on_request_finish(&self, event: &RequestFinish<'_>) {
            let RequestFinishReason::Failed { error } = event.reason else {
                panic!("expected original error, got {:?}", event.reason);
            };
            assert_original(error, &self.source);
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn assert_original(error: &Error, source: &Arc<()>) {
        let Error::Other(error) = error else {
            panic!("original Error variant was lost");
        };
        assert!(Arc::ptr_eq(
            &error.downcast_ref::<Marker>().unwrap().0,
            source
        ));
    }

    struct ErrorBody(Option<Error>);
    impl http_body::Body for ErrorBody {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
            std::task::Poll::Ready(self.0.take().map(Err))
        }
    }

    fn setup() -> (Recorder, crate::response::Response) {
        let source = Arc::new(());
        let body = ErrorBody(Some(Error::Other(Box::new(Marker(source.clone()))))).boxed_unsync();
        let response = crate::response::Response::from_boxed(
            http::Response::new(body),
            "http://localhost/".parse().unwrap(),
        );
        (
            Recorder {
                source,
                errors: Arc::new(AtomicU64::new(0)),
            },
            response,
        )
    }

    fn poll_error<B: http_body::Body<Data = Bytes, Error = Error> + Unpin>(body: &mut B) -> Error {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::pin::Pin::new(body).poll_frame(&mut cx) {
            std::task::Poll::Ready(Some(Err(error))) => error,
            _ => panic!("expected the body error"),
        }
    }

    #[test]
    fn send_body_error_borrows_original_source_and_finishes_once() {
        let (recorder, mut response) = setup();
        let method = http::Method::GET;
        let uri = "http://localhost/".parse().unwrap();
        let mut handle = SendRequestObservationHandle::new(
            &recorder,
            RequestInfo {
                method: &method,
                initial_uri: &uri,
            },
            None,
        )
        .unwrap();
        handle.observe_headers(ResponseInfo {
            status: response.status(),
            version: Some(response.version()),
            source: super::super::ResponseSource::Transport,
        });
        handle.attach_to_send_response(&mut response, false);
        drop(handle);
        let error = poll_error(response.body_mut());
        assert_original(&error, &recorder.source);
        drop(response);
        assert_eq!(recorder.errors.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn local_body_error_borrows_original_source_and_finishes_once() {
        let (recorder, response) = setup();
        let mut response = response.into_local();
        let method = http::Method::GET;
        let uri = "http://localhost/".parse().unwrap();
        let mut handle = LocalRequestObservationHandle::new(
            &recorder,
            RequestInfo {
                method: &method,
                initial_uri: &uri,
            },
            None,
        )
        .unwrap();
        handle.observe_headers(ResponseInfo {
            status: response.status(),
            version: Some(response.version()),
            source: super::super::ResponseSource::Transport,
        });
        handle.attach_to_local_response(&mut response, false);
        drop(handle);
        let error = poll_error(response.body_mut());
        assert_original(&error, &recorder.source);
        drop(response);
        assert_eq!(recorder.errors.load(Ordering::Relaxed), 1);
    }
}
