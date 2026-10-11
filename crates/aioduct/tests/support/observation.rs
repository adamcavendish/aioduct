//! Owned snapshots for integration tests of the typed 0.3 callbacks.

#![allow(dead_code)] // Each integration binary uses a different subset.

use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct TestObserver {
    pub events: Arc<Mutex<Vec<String>>>,
    pub conn_events: Arc<Mutex<Vec<String>>>,
}

impl TestObserver {
    pub fn phases(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn record(&self, fact: String) {
        self.events.lock().unwrap().push(fact);
    }
}

impl aioduct::Observer for TestObserver {
    type Context = ();
    type Observation = Self;
    type ConnectionObservation = Self;

    fn begin(&self, _: aioduct::RequestInfo<'_>, _: Option<()>) -> (aioduct::Interest, Self) {
        use aioduct::Interest;
        (
            Interest::REQUEST_START
                | Interest::RESPONSE_HEADERS
                | Interest::REQUEST_FINISH
                | Interest::ATTEMPT
                | Interest::CONNECTION_ACQUISITION
                | Interest::RESPONSE_BODY,
            self.clone(),
        )
    }

    fn begin_connection(&self) -> (aioduct::Interest, Self) {
        (
            aioduct::Interest::CONNECTION_LIFECYCLE | aioduct::Interest::CONNECTION_ESTABLISHMENT,
            self.clone(),
        )
    }
}

impl aioduct::RequestObservation for TestObserver {
    fn on_request_start(&self, _: &aioduct::RequestStart<'_>) {
        self.record("RequestStart".into());
    }
    fn on_response_headers(&self, _: &aioduct::ResponseHeaders) {
        self.record("ResponseHeaders".into());
    }
    fn on_request_finish(&self, event: &aioduct::RequestFinish<'_>) {
        self.record(format!("RequestFinish({:?})", event.reason));
    }
    fn on_attempt(&self, event: &aioduct::AttemptEvent<'_>) {
        use aioduct::AttemptEvent;
        match event {
            AttemptEvent::Started { .. } => self.record("AttemptStarted".into()),
            AttemptEvent::RetryScheduled { reason, .. } => {
                self.record(format!("RetryScheduled({reason:?})"))
            }
            AttemptEvent::Failed { .. } => self.record("AttemptFailed".into()),
            AttemptEvent::ResponseDisposition { disposition, .. } => {
                self.record(format!("ResponseDisposition({disposition:?})"))
            }
            _ => {}
        }
    }
    fn on_connection_acquisition(&self, event: &aioduct::ConnectionAcquisitionEvent<'_>) {
        use aioduct::ConnectionAcquisitionEvent;
        match event {
            ConnectionAcquisitionEvent::PoolLookup { result, .. } => {
                self.record(format!("PoolLookup({result:?})"))
            }
            ConnectionAcquisitionEvent::PhaseCompleted { phase, .. } => {
                self.record(format!("AcquisitionComplete({phase:?})"))
            }
            ConnectionAcquisitionEvent::PhaseSkipped { phase, reason, .. } => {
                self.record(format!("AcquisitionSkipped({phase:?}, {reason:?})"))
            }
            _ => {}
        }
    }
    fn on_response_body(&self, event: &aioduct::ResponseBodyEvent<'_>) {
        if matches!(event, aioduct::ResponseBodyEvent::Ended { .. }) {
            self.record("ResponseBodyEnded".into());
        }
    }
}

impl aioduct::ConnectionObservation for TestObserver {
    fn on_connection(&self, event: &aioduct::ConnectionObservationEvent<'_>) {
        use aioduct::ConnectionObservationEvent;
        self.conn_events.lock().unwrap().push(format!("{event:?}"));
        match event {
            ConnectionObservationEvent::Established { .. } => self.record("Established".into()),
            ConnectionObservationEvent::PhaseCompleted { phase, .. } => {
                self.record(format!("EstablishmentComplete({phase:?})"))
            }
            _ => {}
        }
    }
}

/// Select a request-only category for a cloneable test receiver.
#[macro_export]
macro_rules! impl_test_observer {
    ($ty:ty, $interest:expr) => {
        impl aioduct::Observer for $ty {
            type Context = ();
            type Observation = Self;
            type ConnectionObservation = aioduct::NoopConnectionObservation;
            fn begin(
                &self,
                _: aioduct::RequestInfo<'_>,
                _: Option<()>,
            ) -> (aioduct::Interest, Self) {
                ($interest, self.clone())
            }
            fn begin_connection(&self) -> (aioduct::Interest, Self::ConnectionObservation) {
                (aioduct::Interest::EMPTY, aioduct::NoopConnectionObservation)
            }
        }
    };
}

/// A cloneable fixture whose physical receiver is independent from requests.
#[macro_export]
macro_rules! impl_connection_observer {
    ($ty:ty) => {
        impl aioduct::Observer for $ty {
            type Context = ();
            type Observation = aioduct::NoopRequestObservation;
            type ConnectionObservation = Self;
            fn begin(
                &self,
                _: aioduct::RequestInfo<'_>,
                _: Option<()>,
            ) -> (aioduct::Interest, Self::Observation) {
                (aioduct::Interest::EMPTY, aioduct::NoopRequestObservation)
            }
            fn begin_connection(&self) -> (aioduct::Interest, Self) {
                (
                    aioduct::Interest::CONNECTION_ESTABLISHMENT
                        | aioduct::Interest::CONNECTION_LIFECYCLE,
                    self.clone(),
                )
            }
        }
    };
}
