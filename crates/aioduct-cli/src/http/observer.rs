//! CLI-owned display snapshots of borrowed library callbacks.
use aioduct::observer::{
    AttemptEvent, ConnectionAcquisitionEvent, ConnectionObservation, ConnectionObservationEvent,
    Instant, Interest, Observer, RequestFinish, RequestFinishReason, RequestInfo,
    RequestObservation, RequestStart, ResponseHeaders, ResponseInfo,
};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum CliEvent {
    Started {
        method: String,
        uri: String,
    },
    ResponseHeaders {
        response: ResponseInfo,
        elapsed: Duration,
    },
    Finished {
        reason: String,
        failed: bool,
        elapsed: Duration,
        original_data_bytes: Option<u64>,
    },
    Diagnostic {
        label: &'static str,
        detail: String,
        failed: bool,
    },
}

/// An absent channel is the non-verbose, empty-interest provider.
#[derive(Default)]
pub struct CliObserver {
    tx: Option<mpsc::UnboundedSender<CliEvent>>,
}

pub struct CliObservation {
    inner: Option<(mpsc::UnboundedSender<CliEvent>, Instant)>,
}

pub struct CliConnectionObservation {
    tx: Option<mpsc::UnboundedSender<CliEvent>>,
}

impl CliObserver {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<CliEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx: Some(tx) }, rx)
    }
}

impl Observer for CliObserver {
    type Context = ();
    type Observation = CliObservation;
    type ConnectionObservation = CliConnectionObservation;
    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self::Observation) {
        let Some(tx) = self.tx.as_ref() else {
            return (Interest::EMPTY, CliObservation { inner: None });
        };
        (
            Interest::REQUEST_START
                | Interest::RESPONSE_HEADERS
                | Interest::REQUEST_FINISH
                | Interest::RESPONSE_BODY_BYTES
                | Interest::ATTEMPT
                | Interest::CONNECTION_ACQUISITION,
            CliObservation {
                inner: Some((tx.clone(), Instant::now())),
            },
        )
    }

    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        let interest = if self.tx.is_some() {
            Interest::CONNECTION_ESTABLISHMENT | Interest::CONNECTION_LIFECYCLE
        } else {
            Interest::EMPTY
        };
        (
            interest,
            CliConnectionObservation {
                tx: self.tx.clone(),
            },
        )
    }
}

impl RequestObservation for CliObservation {
    fn on_request_start(&self, event: &RequestStart<'_>) {
        if let Some((tx, _)) = &self.inner {
            let _ = tx.send(CliEvent::Started {
                method: event.method.to_string(),
                uri: event.initial_uri.to_string(),
            });
        }
    }

    fn on_response_headers(&self, event: &ResponseHeaders) {
        if let Some((tx, started_at)) = &self.inner {
            let _ = tx.send(CliEvent::ResponseHeaders {
                response: event.response,
                elapsed: event.at.duration_since(*started_at).into(),
            });
        }
    }

    fn on_request_finish(&self, event: &RequestFinish<'_>) {
        if let Some((tx, started_at)) = &self.inner {
            let _ = tx.send(CliEvent::Finished {
                reason: format!("{:?}", event.reason),
                failed: matches!(event.reason, RequestFinishReason::Failed { .. }),
                elapsed: event.at.duration_since(*started_at).into(),
                original_data_bytes: event.original_data_bytes,
            });
        }
    }

    fn on_attempt(&self, event: &AttemptEvent<'_>) {
        if let Some((tx, _)) = &self.inner {
            let _ = tx.send(CliEvent::Diagnostic {
                label: "ATTEMPT",
                detail: format!("{event:?}"),
                failed: matches!(event, AttemptEvent::Failed { .. }),
            });
        }
    }

    fn on_connection_acquisition(&self, event: &ConnectionAcquisitionEvent<'_>) {
        if let Some((tx, _)) = &self.inner {
            let _ = tx.send(CliEvent::Diagnostic {
                label: "ACQUIRE",
                detail: format!("{event:?}"),
                failed: matches!(event, ConnectionAcquisitionEvent::Failed { .. }),
            });
        }
    }
}

impl ConnectionObservation for CliConnectionObservation {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(CliEvent::Diagnostic {
                label: "CONNECTION",
                detail: format!("{event:?}"),
                failed: matches!(
                    event,
                    ConnectionObservationEvent::EstablishmentFailed { .. }
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Method, Uri};
    fn info() -> RequestInfo<'static> {
        static URI: std::sync::LazyLock<Uri> =
            std::sync::LazyLock::new(|| Uri::from_static("http://example.com"));
        RequestInfo {
            method: &Method::GET,
            initial_uri: &URI,
        }
    }
    #[test]
    fn events_flow_through_channel() {
        let (observer, mut rx) = CliObserver::new();
        let (_, observation) = observer.begin(info(), None);
        observation.on_request_start(&RequestStart {
            method: info().method,
            initial_uri: info().initial_uri,
            at: Instant::now(),
        });
        assert!(matches!(rx.try_recv().unwrap(), CliEvent::Started { .. }));
    }
    #[test]
    fn non_verbose_does_not_create_a_receiver() {
        let (interest, observation) = CliObserver::default().begin(info(), None);
        assert!(interest.is_empty());
        assert!(observation.inner.is_none());
    }
    #[test]
    fn dropped_receiver_does_not_panic() {
        let (observer, rx) = CliObserver::new();
        drop(rx);
        let (_, observation) = observer.begin(info(), None);
        observation.on_request_start(&RequestStart {
            method: info().method,
            initial_uri: info().initial_uri,
            at: Instant::now(),
        });
    }
}
