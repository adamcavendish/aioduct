//! Private connection callback admission and terminal responsibility.

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use super::{
    ConnectAttempt, ConnectAttemptObserver, ConnectionAssociation, ConnectionObservation,
    ConnectionObservationEvent, Instant, Interest, Observer,
};
use crate::Error;
use std::sync::atomic::{AtomicU8, Ordering};

fn admit_establishment(state: &AtomicU8, event: &ConnectionObservationEvent<'_>) -> bool {
    let terminal = match event {
        ConnectionObservationEvent::Established { .. } => 1,
        ConnectionObservationEvent::EstablishmentFailed { .. }
        | ConnectionObservationEvent::EstablishmentCancelled { .. } => 2,
        _ => return true,
    };
    state
        .compare_exchange(0, terminal, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

pub(crate) trait EstablishmentReceiver {
    fn fail(&self, error: &Error);
    fn cancel(&self);
}

/// This guard belongs outside the establishment deadline, never to a nested
/// handshake. Its owner remains available when a timeout drops that handshake.
pub(crate) struct EstablishmentScope<T: EstablishmentReceiver>(T);

impl<T: EstablishmentReceiver> EstablishmentScope<T> {
    pub(crate) fn failed(&self, error: &Error) {
        self.0.fail(error);
    }

    pub(crate) fn result<U>(&self, result: &Result<U, Error>) {
        if let Err(error) = result {
            self.0.fail(error);
        }
    }
}

impl<T: EstablishmentReceiver> Drop for EstablishmentScope<T> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

macro_rules! establishment_receiver {
    ($ty:ty) => {
        impl EstablishmentReceiver for $ty {
            fn fail(&self, error: &Error) {
                self.establishment_failed(error);
            }

            fn cancel(&self) {
                if self.interest().contains(Interest::CONNECTION_ESTABLISHMENT)
                    && self.0.establishment.load(Ordering::Relaxed) == 0
                {
                    self.emit(&ConnectionObservationEvent::EstablishmentCancelled {
                        at: Instant::now(),
                    });
                }
            }
        }

        impl $ty {
            pub(crate) fn establishment_scope(&self) -> Option<EstablishmentScope<Self>> {
                self.interest()
                    .contains(Interest::CONNECTION_ESTABLISHMENT)
                    .then(|| EstablishmentScope(self.clone()))
            }
        }
    };
}

establishment_receiver!(ConnectionObservationOwner);
establishment_receiver!(LocalConnectionObservationOwner);

macro_rules! connect_attempt_observer {
    ($ty:ty) => {
        impl ConnectAttemptObserver for $ty {
            fn connect_attempt_started(&self, attempt: ConnectAttempt) {
                if self.interest().contains(Interest::CONNECTION_ESTABLISHMENT) {
                    self.emit(&ConnectionObservationEvent::ConnectAttemptStarted {
                        attempt,
                        at: Instant::now(),
                    });
                }
            }

            fn connect_attempt_succeeded(&self, attempt: ConnectAttempt) {
                if self.interest().contains(Interest::CONNECTION_ESTABLISHMENT) {
                    self.emit(&ConnectionObservationEvent::ConnectAttemptSucceeded {
                        attempt,
                        at: Instant::now(),
                    });
                }
            }

            fn connect_attempt_failed(&self, attempt: ConnectAttempt, error: &Error) {
                if self.interest().contains(Interest::CONNECTION_ESTABLISHMENT) {
                    self.emit(&ConnectionObservationEvent::ConnectAttemptFailed {
                        attempt,
                        error,
                        at: Instant::now(),
                    });
                }
            }

            fn connect_attempt_cancelled(&self, attempt: ConnectAttempt) {
                if self.interest().contains(Interest::CONNECTION_ESTABLISHMENT) {
                    self.emit(&ConnectionObservationEvent::ConnectAttemptCancelled {
                        attempt,
                        at: Instant::now(),
                    });
                }
            }
        }
    };
}

connect_attempt_observer!(ConnectionObservationOwner);
connect_attempt_observer!(LocalConnectionObservationOwner);

/// A connection-local candidate scope whose drop means that the candidate was
/// cancelled before its underlying connect completed.
pub(crate) struct ConnectAttemptScope<T: ConnectAttemptObserver + Clone> {
    observer: T,
    attempt: ConnectAttempt,
    completed: bool,
}

impl<T: ConnectAttemptObserver + Clone> ConnectAttemptScope<T> {
    pub(crate) fn succeeded(&mut self) {
        if !self.completed {
            self.completed = true;
            self.observer.connect_attempt_succeeded(self.attempt);
        }
    }

    pub(crate) fn failed(&mut self, error: &Error) {
        if !self.completed {
            self.completed = true;
            self.observer.connect_attempt_failed(self.attempt, error);
        }
    }
}

impl<T: ConnectAttemptObserver + Clone> Drop for ConnectAttemptScope<T> {
    fn drop(&mut self) {
        if !self.completed {
            self.completed = true;
            self.observer.connect_attempt_cancelled(self.attempt);
        }
    }
}

macro_rules! connect_attempt_scope {
    ($ty:ty) => {
        impl $ty {
            pub(crate) fn connect_attempt_scope(
                &self,
                attempt: ConnectAttempt,
            ) -> Option<ConnectAttemptScope<Self>> {
                if !self.interest().contains(Interest::CONNECTION_ESTABLISHMENT) {
                    return None;
                }
                self.connect_attempt_started(attempt);
                Some(ConnectAttemptScope {
                    observer: self.clone(),
                    attempt,
                    completed: false,
                })
            }
        }
    };
}

connect_attempt_scope!(ConnectionObservationOwner);
connect_attempt_scope!(LocalConnectionObservationOwner);

enum Terminal {
    Closed { error: Option<Error>, at: Instant },
    Detached { at: Instant },
}

#[derive(Default)]
struct State {
    active: usize,
    ending: bool,
    pending: Option<Terminal>,
    reusable: Option<bool>,
    revision: u64,
}

impl State {
    fn enter(&mut self) -> bool {
        if self.ending {
            return false;
        }
        self.active = self
            .active
            .checked_add(1)
            .unwrap_or_else(|| unreachable!("connection callback admission overflow"));
        true
    }

    fn ready(&mut self) -> Option<Terminal> {
        if self.active == 0 {
            self.pending.take()
        } else {
            None
        }
    }

    fn leave(&mut self) -> Option<Terminal> {
        self.active -= 1;
        self.ready()
    }

    fn end(&mut self, incoming: &mut Option<Terminal>) -> Option<Terminal> {
        if self.ending {
            return None;
        }
        self.ending = true;
        self.pending = incoming.take();
        self.ready()
    }

    fn eligibility(&mut self, reusable: bool) -> Option<u64> {
        if self.ending || self.reusable == Some(reusable) {
            return None;
        }
        // A transport which never became reusable has no loss to report.
        if self.reusable.is_none() && !reusable {
            return None;
        }
        if !self.enter() {
            return None;
        }
        self.reusable = Some(reusable);
        self.revision = self
            .revision
            .checked_add(1)
            .unwrap_or_else(|| unreachable!("connection eligibility revision overflow"));
        Some(self.revision)
    }
}

fn notify(observation: &dyn ConnectionObservation, terminal: Terminal) {
    match terminal {
        Terminal::Closed { error, at } => {
            observation.on_connection(&ConnectionObservationEvent::Closed {
                error: error.as_ref(),
                at,
            })
        }
        Terminal::Detached { at } => {
            observation.on_connection(&ConnectionObservationEvent::Detached { at })
        }
    }
}

struct SendInner {
    interest: Interest,
    observation: Box<dyn ConnectionObservation + Send + Sync>,
    // Only multi-producer lifecycle subscribers pay for admission.
    state: Option<parking_lot::Mutex<State>>,
    establishment: AtomicU8,
}

#[derive(Clone)]
pub(crate) struct ConnectionObservationOwner(Arc<SendInner>);

impl ConnectionObservationOwner {
    pub(crate) fn send<O>(observer: &O) -> Option<Self>
    where
        O: Observer + Send + Sync,
        O::ConnectionObservation: Send + Sync,
    {
        let (interest, observation) = observer.begin_connection();
        if !interest.intersects(Interest::CONNECTION_LIFECYCLE | Interest::CONNECTION_ESTABLISHMENT)
        {
            return None;
        }
        Some(Self(Arc::new(SendInner {
            interest,
            observation: Box::new(observation),
            establishment: AtomicU8::new(0),
            state: interest
                .contains(Interest::CONNECTION_LIFECYCLE)
                .then(|| parking_lot::Mutex::new(State::default())),
        })))
    }

    pub(crate) fn interest(&self) -> Interest {
        self.0.interest
    }

    pub(crate) fn emit(&self, event: &ConnectionObservationEvent<'_>) {
        if !admit_establishment(&self.0.establishment, event) {
            return;
        }
        let lease = if let Some(state) = &self.0.state {
            if !state.lock().enter() {
                return;
            }
            Some(SendLease(&self.0))
        } else {
            None
        };
        self.0.observation.on_connection(event);
        drop(lease);
    }

    pub(crate) fn establishment_failed(&self, error: &Error) {
        if self.interest().contains(Interest::CONNECTION_ESTABLISHMENT) {
            self.emit(&ConnectionObservationEvent::EstablishmentFailed {
                error,
                at: Instant::now(),
            });
        }
    }

    pub(crate) fn closed(&self, error: Option<Error>) {
        self.closed_at(error, Instant::now());
    }

    pub(crate) fn closed_at(&self, error: Option<Error>, at: Instant) {
        let Some(state) = &self.0.state else {
            return;
        };
        let mut incoming = Some(Terminal::Closed { error, at });
        let ready = state.lock().end(&mut incoming);
        // Both rejected errors and terminal callbacks stay outside the lock.
        if let Some(terminal) = ready {
            notify(&*self.0.observation, terminal);
        }
    }

    pub(crate) fn detached_at(&self, at: Instant) {
        let Some(state) = &self.0.state else {
            return;
        };
        let mut incoming = Some(Terminal::Detached { at });
        let ready = state.lock().end(&mut incoming);
        if let Some(terminal) = ready {
            notify(&*self.0.observation, terminal);
        }
    }

    pub(crate) fn reserve_eligibility(&self, reusable: bool) -> Option<EligibilityChange> {
        let revision = self.0.state.as_ref()?.lock().eligibility(reusable)?;
        Some(EligibilityChange {
            receiver: EligibilityReceiver::Send(self.clone()),
            reusable,
            revision,
            at: Instant::now(),
        })
    }
}

struct SendLease<'a>(&'a SendInner);
impl Drop for SendLease<'_> {
    fn drop(&mut self) {
        let ready = self.0.state.as_ref().and_then(|state| state.lock().leave());
        if let Some(terminal) = ready {
            notify(&*self.0.observation, terminal);
        }
    }
}

struct LocalInner {
    interest: Interest,
    observation: Box<dyn ConnectionObservation>,
    state: Option<RefCell<State>>,
    establishment: AtomicU8,
}

#[derive(Clone)]
pub(crate) struct LocalConnectionObservationOwner(Rc<LocalInner>);

impl LocalConnectionObservationOwner {
    pub(crate) fn local<O: Observer>(observer: &O) -> Option<Self> {
        let (interest, observation) = observer.begin_connection();
        if !interest.intersects(Interest::CONNECTION_LIFECYCLE | Interest::CONNECTION_ESTABLISHMENT)
        {
            return None;
        }
        Some(Self(Rc::new(LocalInner {
            interest,
            observation: Box::new(observation),
            establishment: AtomicU8::new(0),
            state: interest
                .contains(Interest::CONNECTION_LIFECYCLE)
                .then(|| RefCell::new(State::default())),
        })))
    }

    pub(crate) fn interest(&self) -> Interest {
        self.0.interest
    }

    pub(crate) fn closed_at(&self, error: Option<Error>, at: Instant) {
        self.0.closed_at(error, at);
    }

    pub(crate) fn detached_at(&self, at: Instant) {
        let Some(state) = &self.0.state else {
            return;
        };
        let mut incoming = Some(Terminal::Detached { at });
        let ready = state.borrow_mut().end(&mut incoming);
        if let Some(terminal) = ready {
            notify(&*self.0.observation, terminal);
        }
    }

    pub(crate) fn emit(&self, event: &ConnectionObservationEvent<'_>) {
        self.0.emit(event);
    }

    pub(crate) fn establishment_failed(&self, error: &Error) {
        if self.interest().contains(Interest::CONNECTION_ESTABLISHMENT) {
            self.emit(&ConnectionObservationEvent::EstablishmentFailed {
                error,
                at: Instant::now(),
            });
        }
    }

    pub(crate) fn register(self, connection: ConnectionAssociation) {
        let old = LOCAL_CONNECTION_OBSERVATIONS
            .with(|observations| observations.borrow_mut().insert(connection, self.0));
        drop(old);
    }

    pub(crate) fn emit_registered(
        connection: ConnectionAssociation,
        event: &ConnectionObservationEvent<'_>,
    ) {
        let observation = LOCAL_CONNECTION_OBSERVATIONS
            .with(|observations| observations.borrow().get(&connection).cloned());
        if let Some(observation) = observation {
            observation.emit(event);
        }
    }

    pub(crate) fn close_registered(connection: ConnectionAssociation, error: Option<Error>) {
        let observation = LOCAL_CONNECTION_OBSERVATIONS
            .with(|observations| observations.borrow().get(&connection).cloned());
        if let Some(observation) = observation {
            observation.closed(error);
        }
    }

    pub(crate) fn unregister(connection: ConnectionAssociation) {
        let observation = LOCAL_CONNECTION_OBSERVATIONS
            .with(|observations| observations.borrow_mut().remove(&connection));
        drop(observation);
    }

    pub(crate) fn reserve_registered_eligibility(
        connection: ConnectionAssociation,
        reusable: bool,
    ) -> Option<EligibilityChange> {
        let receiver = LOCAL_CONNECTION_OBSERVATIONS
            .with(|observations| observations.borrow().get(&connection).cloned())?;
        let revision = receiver
            .state
            .as_ref()?
            .borrow_mut()
            .eligibility(reusable)?;
        Some(EligibilityChange {
            receiver: EligibilityReceiver::Local(receiver),
            reusable,
            revision,
            at: Instant::now(),
        })
    }
}

impl LocalInner {
    fn emit(&self, event: &ConnectionObservationEvent<'_>) {
        if !admit_establishment(&self.establishment, event) {
            return;
        }
        let lease = if let Some(state) = &self.state {
            if !state.borrow_mut().enter() {
                return;
            }
            Some(LocalLease(self))
        } else {
            None
        };
        self.observation.on_connection(event);
        drop(lease);
    }

    fn closed(&self, error: Option<Error>) {
        self.closed_at(error, Instant::now());
    }

    fn closed_at(&self, error: Option<Error>, at: Instant) {
        let Some(state) = &self.state else {
            return;
        };
        let mut incoming = Some(Terminal::Closed { error, at });
        let ready = state.borrow_mut().end(&mut incoming);
        if let Some(terminal) = ready {
            notify(&*self.observation, terminal);
        }
    }
}

struct LocalLease<'a>(&'a LocalInner);
impl Drop for LocalLease<'_> {
    fn drop(&mut self) {
        let ready = self
            .0
            .state
            .as_ref()
            .and_then(|state| state.borrow_mut().leave());
        if let Some(terminal) = ready {
            notify(&*self.0.observation, terminal);
        }
    }
}

enum EligibilityReceiver {
    Send(ConnectionObservationOwner),
    Local(Rc<LocalInner>),
}

/// Admission happens at the actual pool state commit. Delivery and even
/// abandoning a committed batch happen only after releasing the pool lock.
pub(crate) struct EligibilityChange {
    receiver: EligibilityReceiver,
    reusable: bool,
    revision: u64,
    at: Instant,
}

impl EligibilityChange {
    pub(crate) fn dispatch(self) {
        let event = ConnectionObservationEvent::PoolEligibilityChanged {
            reusable: self.reusable,
            revision: self.revision,
            at: self.at,
        };
        match &self.receiver {
            EligibilityReceiver::Send(receiver) => receiver.0.observation.on_connection(&event),
            EligibilityReceiver::Local(receiver) => receiver.observation.on_connection(&event),
        }
    }
}

impl Drop for EligibilityChange {
    fn drop(&mut self) {
        match &self.receiver {
            EligibilityReceiver::Send(receiver) => drop(SendLease(&receiver.0)),
            EligibilityReceiver::Local(receiver) => drop(LocalLease(receiver)),
        }
    }
}

thread_local! {
    static LOCAL_CONNECTION_OBSERVATIONS: RefCell<HashMap<ConnectionAssociation, Rc<LocalInner>>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
mod tests;
