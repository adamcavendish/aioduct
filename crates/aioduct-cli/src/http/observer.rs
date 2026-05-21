use aioduct::observer::{ConnectionEvent, RequestEvent, RequestObserver};
use tokio::sync::mpsc;

pub struct CliObserver {
    tx: mpsc::UnboundedSender<RequestEvent>,
}

impl CliObserver {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<RequestEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

impl RequestObserver for CliObserver {
    fn on_event(&self, event: &RequestEvent) {
        let _ = self.tx.send(event.clone());
    }

    fn on_connection_event(&self, _event: &ConnectionEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use aioduct::observer::{Instant, RequestPhase};
    use http::{Method, Uri};

    #[test]
    fn events_flow_through_channel() {
        let (observer, mut rx) = CliObserver::new();
        let event = RequestEvent {
            method: Method::GET,
            uri: Uri::from_static("http://example.com"),
            phase: RequestPhase::Started,
            at: Instant::now(),
        };
        observer.on_event(&event);
        let received = rx.try_recv().unwrap();
        assert_eq!(received.phase, RequestPhase::Started);
    }

    #[test]
    fn dropped_receiver_does_not_panic() {
        let (observer, rx) = CliObserver::new();
        drop(rx);
        let event = RequestEvent {
            method: Method::GET,
            uri: Uri::from_static("http://example.com"),
            phase: RequestPhase::Started,
            at: Instant::now(),
        };
        observer.on_event(&event);
    }
}
