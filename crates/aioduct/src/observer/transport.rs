//! Physical H1 transport responsibility, independent of HTTP and channel owners.

use std::{
    cell::RefCell,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

use super::{ConnectionObservationOwner, Instant, Interest, LocalConnectionObservationOwner};
use crate::Error;

trait Receiver {
    fn closed(&self, error: Option<Error>, at: Instant);
    fn detached(&self, at: Instant);
}
impl Receiver for ConnectionObservationOwner {
    fn closed(&self, error: Option<Error>, at: Instant) {
        self.closed_at(error, at);
    }
    fn detached(&self, at: Instant) {
        self.detached_at(at);
    }
}
impl Receiver for LocalConnectionObservationOwner {
    fn closed(&self, error: Option<Error>, at: Instant) {
        self.closed_at(error, at);
    }
    fn detached(&self, at: Instant) {
        self.detached_at(at);
    }
}

struct State<R> {
    receiver: Option<R>,
    established: bool,
}
enum Delivery<R> {
    Closed {
        receiver: R,
        error: Option<Error>,
        at: Instant,
    },
    Detached {
        receiver: R,
        at: Instant,
    },
}
impl<R: Receiver> Delivery<R> {
    fn dispatch(self) {
        match self {
            Self::Closed {
                receiver,
                error,
                at,
            } => receiver.closed(error, at),
            Self::Detached { receiver, at } => receiver.detached(at),
        }
    }
}
impl<R> State<R> {
    fn new(receiver: R) -> Self {
        Self {
            receiver: Some(receiver),
            established: false,
        }
    }
    fn established(&mut self) {
        self.established = true;
    }
    fn driver_ended(&mut self, error: Option<Error>) -> Option<Delivery<R>> {
        if !self.established {
            return None;
        }
        Some(Delivery::Closed {
            receiver: self.receiver.take()?,
            error,
            at: Instant::now(),
        })
    }
    fn released(&mut self) -> Option<Delivery<R>> {
        if !self.established {
            self.receiver.take();
            return None;
        }
        Some(Delivery::Detached {
            receiver: self.receiver.take()?,
            at: Instant::now(),
        })
    }
    fn detach(&mut self) -> Option<Delivery<R>> {
        if !self.established {
            self.receiver.take();
            return None;
        }
        Some(Delivery::Detached {
            receiver: self.receiver.take()?,
            at: Instant::now(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct SendTransportObservation(
    Arc<parking_lot::Mutex<State<ConnectionObservationOwner>>>,
);
impl SendTransportObservation {
    pub(crate) fn new(owner: &ConnectionObservationOwner) -> Option<Self> {
        owner
            .interest()
            .contains(Interest::CONNECTION_LIFECYCLE)
            .then(|| Self(Arc::new(parking_lot::Mutex::new(State::new(owner.clone())))))
    }
    pub(crate) fn established(&self) {
        self.0.lock().established();
    }
    pub(crate) fn driver_ended(&self, mut error: Option<Error>) {
        let ready = self.0.lock().driver_ended(error.take());
        if let Some(ready) = ready {
            ready.dispatch();
        }
        // Rejected errors are also destroyed after unlocking.
    }
    pub(crate) fn detach(&self) {
        let ready = self.0.lock().detach();
        if let Some(ready) = ready {
            ready.dispatch();
        }
    }
}

#[derive(Clone)]
pub(crate) struct LocalTransportObservation(Rc<RefCell<State<LocalConnectionObservationOwner>>>);
impl LocalTransportObservation {
    pub(crate) fn new(owner: &LocalConnectionObservationOwner) -> Option<Self> {
        owner
            .interest()
            .contains(Interest::CONNECTION_LIFECYCLE)
            .then(|| Self(Rc::new(RefCell::new(State::new(owner.clone())))))
    }
    pub(crate) fn established(&self) {
        self.0.borrow_mut().established();
    }
    pub(crate) fn driver_ended(&self, mut error: Option<Error>) {
        let ready = self.0.borrow_mut().driver_ended(error.take());
        if let Some(ready) = ready {
            ready.dispatch();
        }
    }
}

pub(crate) trait TransportRelease {
    fn released(&self);
    fn detach(&self);
}
impl TransportRelease for SendTransportObservation {
    fn released(&self) {
        let ready = self.0.lock().released();
        if let Some(ready) = ready {
            ready.dispatch();
        }
    }
    fn detach(&self) {
        SendTransportObservation::detach(self);
    }
}
impl TransportRelease for LocalTransportObservation {
    fn released(&self) {
        let ready = self.0.borrow_mut().released();
        if let Some(ready) = ready {
            ready.dispatch();
        }
    }
    fn detach(&self) {
        let ready = self.0.borrow_mut().detach();
        if let Some(ready) = ready {
            ready.dispatch();
        }
    }
}

/// Only lifecycle subscribers install this wrapper. Read/write operations
/// merely delegate; no polls, bytes, clocks, or callbacks are tracked here.
pub(crate) struct ObservedIo<T, R: TransportRelease> {
    inner: T,
    observation: R,
}
pub(crate) type SendObservedIo<T> = ObservedIo<T, SendTransportObservation>;
impl<T, R: TransportRelease> ObservedIo<T, R> {
    pub(crate) fn new(inner: T, observation: R) -> Self {
        Self { inner, observation }
    }
    pub(crate) fn into_inner(self) -> T {
        let this = std::mem::ManuallyDrop::new(self);
        this.observation.detach();
        // `this` is never dropped, so moving the transport out is sound.
        unsafe { std::ptr::read(&this.inner) }
    }
}
impl<T, R: TransportRelease> Drop for ObservedIo<T, R> {
    fn drop(&mut self) {
        self.observation.released();
    }
}
impl<T: hyper::rt::Read + Unpin, R: TransportRelease + Unpin> hyper::rt::Read for ObservedIo<T, R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}
impl<T: hyper::rt::Write + Unpin, R: TransportRelease + Unpin> hyper::rt::Write
    for ObservedIo<T, R>
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}

#[cfg(test)]
mod tests {
    use super::{Instant, Receiver, State};
    use crate::Error;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone)]
    struct TestReceiver(Rc<RefCell<Vec<&'static str>>>);

    impl Receiver for TestReceiver {
        fn closed(&self, _error: Option<Error>, _at: Instant) {
            self.0.borrow_mut().push("closed");
        }

        fn detached(&self, _at: Instant) {
            self.0.borrow_mut().push("detached");
        }
    }

    #[test]
    fn clean_driver_completion_is_closed_not_detached() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut state = State::new(TestReceiver(events.clone()));
        state.established();

        state
            .driver_ended(None)
            .expect("clean completion delivery")
            .dispatch();

        assert_eq!(&*events.borrow(), &["closed"]);
        assert!(state.released().is_none());
    }
}
