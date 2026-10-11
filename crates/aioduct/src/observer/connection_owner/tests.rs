use super::*;
use crate::observer::{ConnectionEstablishmentPhase, NoopRequestObservation, RequestInfo};
use std::sync::{
    Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Provider<T>(T, Interest);
impl<T: ConnectionObservation + Clone> Observer for Provider<T> {
    type Context = ();
    type Observation = NoopRequestObservation;
    type ConnectionObservation = T;
    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self::Observation) {
        (Interest::EMPTY, NoopRequestObservation)
    }
    fn begin_connection(&self) -> (Interest, T) {
        (self.1, self.0.clone())
    }
}

#[derive(Clone, Default)]
struct EstablishmentRecorder(Arc<Mutex<Vec<&'static str>>>);

impl ConnectionObservation for EstablishmentRecorder {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        let fact = match event {
            ConnectionObservationEvent::EstablishmentFailed { error, .. } => {
                assert!(matches!(error, Error::ConnectTimeout));
                "failed"
            }
            ConnectionObservationEvent::EstablishmentCancelled { .. } => "cancelled",
            _ => return,
        };
        self.0.lock().unwrap().push(fact);
    }
}

#[test]
fn nested_send_establishment_failures_and_scope_drop_have_one_terminal() {
    let recorder = EstablishmentRecorder::default();
    let owner = ConnectionObservationOwner::send(&Provider(
        recorder.clone(),
        Interest::CONNECTION_ESTABLISHMENT,
    ))
    .unwrap();
    let scope = owner.establishment_scope().unwrap();
    let error = Error::ConnectTimeout;
    owner.establishment_failed(&error);
    owner.establishment_failed(&error);
    scope.result(&Err::<(), _>(error));
    drop(scope);
    assert_eq!(*recorder.0.lock().unwrap(), vec!["failed"]);
}

#[test]
fn nested_local_establishment_failures_and_scope_drop_have_one_terminal() {
    let recorder = EstablishmentRecorder::default();
    let owner = LocalConnectionObservationOwner::local(&Provider(
        recorder.clone(),
        Interest::CONNECTION_ESTABLISHMENT,
    ))
    .unwrap();
    let scope = owner.establishment_scope().unwrap();
    let error = Error::ConnectTimeout;
    owner.establishment_failed(&error);
    owner.establishment_failed(&error);
    scope.result(&Err::<(), _>(error));
    drop(scope);
    assert_eq!(*recorder.0.lock().unwrap(), vec!["failed"]);
}

#[derive(Clone)]
struct ConcurrentReceiver {
    started: Arc<Barrier>,
    released: Arc<Barrier>,
    active: Arc<AtomicUsize>,
    terminal: Arc<AtomicUsize>,
}
impl ConnectionObservation for ConcurrentReceiver {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        match event {
            ConnectionObservationEvent::PhaseStarted { .. } => {
                assert_eq!(self.terminal.load(Ordering::SeqCst), 0);
                self.active.fetch_add(1, Ordering::SeqCst);
                self.started.wait();
                self.released.wait();
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
            ConnectionObservationEvent::Closed { .. } => {
                assert_eq!(self.active.load(Ordering::SeqCst), 0);
                assert_eq!(self.terminal.fetch_add(1, Ordering::SeqCst), 0);
            }
            _ => {}
        }
    }
}

#[test]
fn terminal_waits_for_all_64_admitted_callbacks_without_blocking_close() {
    let receiver = ConcurrentReceiver {
        started: Arc::new(Barrier::new(65)),
        released: Arc::new(Barrier::new(65)),
        active: Arc::new(AtomicUsize::new(0)),
        terminal: Arc::new(AtomicUsize::new(0)),
    };
    let owner = ConnectionObservationOwner::send(&Provider(
        receiver.clone(),
        Interest::CONNECTION_LIFECYCLE | Interest::CONNECTION_ESTABLISHMENT,
    ))
    .unwrap();
    let threads: Vec<_> = (0..64)
        .map(|_| {
            let owner = owner.clone();
            std::thread::spawn(move || {
                owner.emit(&ConnectionObservationEvent::PhaseStarted {
                    phase: ConnectionEstablishmentPhase::Dns,
                    at: Instant::now(),
                })
            })
        })
        .collect();
    receiver.started.wait();
    owner.closed(None);
    owner.closed(None);
    assert_eq!(receiver.terminal.load(Ordering::SeqCst), 0);
    receiver.released.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(receiver.terminal.load(Ordering::SeqCst), 1);
    // New events are rejected after terminal notification.
    owner.emit(&ConnectionObservationEvent::PhaseStarted {
        phase: ConnectionEstablishmentPhase::Dns,
        at: Instant::now(),
    });
}

#[derive(Clone)]
struct ReentrantSend {
    owner: Arc<Mutex<Option<ConnectionObservationOwner>>>,
    facts: Arc<Mutex<Vec<&'static str>>>,
}
impl ConnectionObservation for ReentrantSend {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        match event {
            ConnectionObservationEvent::Established { .. } => {
                self.facts.lock().unwrap().push("enter");
                let owner = self.owner.lock().unwrap().as_ref().unwrap().clone();
                owner.closed(None);
                self.facts.lock().unwrap().push("leave");
            }
            ConnectionObservationEvent::Closed { .. } => self.facts.lock().unwrap().push("closed"),
            _ => {}
        }
    }
}

#[test]
fn close_reentry_defers_terminal_until_the_current_callback_returns() {
    let receiver = ReentrantSend {
        owner: Arc::new(Mutex::new(None)),
        facts: Arc::new(Mutex::new(Vec::new())),
    };
    let owner = ConnectionObservationOwner::send(&Provider(
        receiver.clone(),
        Interest::CONNECTION_LIFECYCLE,
    ))
    .unwrap();
    *receiver.owner.lock().unwrap() = Some(owner.clone());
    owner.emit(&ConnectionObservationEvent::Established {
        connection: ConnectionAssociation::allocate(),
        version: http::Version::HTTP_2,
        at: Instant::now(),
    });
    assert_eq!(
        *receiver.facts.lock().unwrap(),
        ["enter", "leave", "closed"]
    );
    receiver.owner.lock().unwrap().take();
}

#[derive(Clone)]
struct LocalReentry {
    connection: ConnectionAssociation,
    facts: Rc<RefCell<Vec<&'static str>>>,
}
impl ConnectionObservation for LocalReentry {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        match event {
            ConnectionObservationEvent::Established { .. } => {
                self.facts.borrow_mut().push("enter");
                LocalConnectionObservationOwner::close_registered(self.connection, None);
                LocalConnectionObservationOwner::unregister(self.connection);
                self.facts.borrow_mut().push("leave");
            }
            ConnectionObservationEvent::Closed { .. } => self.facts.borrow_mut().push("closed"),
            _ => {}
        }
    }
}

#[test]
fn local_registry_and_gate_borrows_are_released_before_reentrant_user_code() {
    let connection = ConnectionAssociation::allocate();
    let facts = Rc::new(RefCell::new(Vec::new()));
    let receiver = LocalReentry {
        connection,
        facts: facts.clone(),
    };
    LocalConnectionObservationOwner::local(&Provider(receiver, Interest::CONNECTION_LIFECYCLE))
        .unwrap()
        .register(connection);
    LocalConnectionObservationOwner::emit_registered(
        connection,
        &ConnectionObservationEvent::Established {
            connection,
            version: http::Version::HTTP_2,
            at: Instant::now(),
        },
    );
    assert_eq!(*facts.borrow(), ["enter", "leave", "closed"]);
}

#[derive(Clone)]
struct Recorder(Arc<Mutex<Vec<(bool, u64)>>>);
impl ConnectionObservation for Recorder {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        if let ConnectionObservationEvent::PoolEligibilityChanged {
            reusable, revision, ..
        } = event
        {
            self.0.lock().unwrap().push((*reusable, *revision));
        }
    }
}

#[test]
fn eligibility_admission_keeps_revisions_and_does_not_invent_reuse() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let owner = ConnectionObservationOwner::send(&Provider(
        Recorder(records.clone()),
        Interest::CONNECTION_LIFECYCLE,
    ))
    .unwrap();
    assert!(owner.reserve_eligibility(false).is_none());
    let first = owner.reserve_eligibility(true).unwrap();
    assert!(owner.reserve_eligibility(true).is_none());
    let second = owner.reserve_eligibility(false).unwrap();
    // Lock-out delivery need not preserve the commit order; revisions do.
    second.dispatch();
    first.dispatch();
    assert_eq!(*records.lock().unwrap(), [(false, 2), (true, 1)]);
    owner.closed(None);
    assert!(owner.reserve_eligibility(true).is_none());
}

#[derive(Debug)]
struct OriginalSource(Arc<AtomicUsize>);
impl std::fmt::Display for OriginalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("original connection error")
    }
}
impl std::error::Error for OriginalSource {}
impl Drop for OriginalSource {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct ErrorRecorder(Arc<AtomicUsize>);
impl ConnectionObservation for ErrorRecorder {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        if let ConnectionObservationEvent::Closed {
            error: Some(Error::Io(error)),
            ..
        } = event
        {
            let source = error
                .get_ref()
                .unwrap()
                .downcast_ref::<OriginalSource>()
                .unwrap();
            self.0
                .store(source as *const OriginalSource as usize, Ordering::SeqCst);
        }
    }
}

#[test]
fn deferred_terminal_borrows_the_original_noncloneable_error_and_drops_it_once() {
    let seen = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let source = Box::new(OriginalSource(drops.clone()));
    let address = &*source as *const OriginalSource as usize;
    let source: Box<dyn std::error::Error + Send + Sync> = source;
    let owner = ConnectionObservationOwner::send(&Provider(
        ErrorRecorder(seen.clone()),
        Interest::CONNECTION_LIFECYCLE,
    ))
    .unwrap();
    let admitted = owner.reserve_eligibility(true).unwrap();
    owner.closed(Some(Error::Io(std::io::Error::other(source))));
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    admitted.dispatch();
    assert_eq!(seen.load(Ordering::SeqCst), address);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

struct ReentrantErrorDrop {
    owner: std::sync::Weak<SendInner>,
    drops: Arc<AtomicUsize>,
}
impl std::fmt::Debug for ReentrantErrorDrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReentrantErrorDrop")
    }
}
impl std::fmt::Display for ReentrantErrorDrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rejected close")
    }
}
impl std::error::Error for ReentrantErrorDrop {}
impl Drop for ReentrantErrorDrop {
    fn drop(&mut self) {
        let owner = ConnectionObservationOwner(self.owner.upgrade().unwrap());
        assert!(owner.reserve_eligibility(true).is_none());
        owner.closed(None);
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn rejected_terminal_error_is_destroyed_outside_the_admission_lock() {
    let owner = ConnectionObservationOwner::send(&Provider(
        Recorder(Arc::new(Mutex::new(Vec::new()))),
        Interest::CONNECTION_LIFECYCLE,
    ))
    .unwrap();
    owner.closed(None);
    let drops = Arc::new(AtomicUsize::new(0));
    owner.closed(Some(Error::Io(std::io::Error::other(ReentrantErrorDrop {
        owner: Arc::downgrade(&owner.0),
        drops: drops.clone(),
    }))));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[derive(Debug)]
struct ReentrantLocalErrorDrop {
    connection: ConnectionAssociation,
    drops: Arc<AtomicUsize>,
}
impl std::fmt::Display for ReentrantLocalErrorDrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rejected local close")
    }
}
impl std::error::Error for ReentrantLocalErrorDrop {}
impl Drop for ReentrantLocalErrorDrop {
    fn drop(&mut self) {
        LocalConnectionObservationOwner::close_registered(self.connection, None);
        LocalConnectionObservationOwner::unregister(self.connection);
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn local_error_destruction_can_reenter_and_unregister_its_receiver() {
    let connection = ConnectionAssociation::allocate();
    LocalConnectionObservationOwner::local(&Provider(
        Recorder(Arc::new(Mutex::new(Vec::new()))),
        Interest::CONNECTION_LIFECYCLE,
    ))
    .unwrap()
    .register(connection);
    LocalConnectionObservationOwner::close_registered(connection, None);
    let drops = Arc::new(AtomicUsize::new(0));
    LocalConnectionObservationOwner::close_registered(
        connection,
        Some(Error::Io(std::io::Error::other(ReentrantLocalErrorDrop {
            connection,
            drops: drops.clone(),
        }))),
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[derive(Clone)]
struct PanicReceiver(Arc<AtomicUsize>);
impl ConnectionObservation for PanicReceiver {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        match event {
            ConnectionObservationEvent::PoolEligibilityChanged { .. } => {
                panic!("user callback panicked")
            }
            ConnectionObservationEvent::Closed { .. } => {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

#[test]
fn callback_unwind_releases_send_and_local_admissions() {
    let closed = Arc::new(AtomicUsize::new(0));
    let provider = Provider(
        PanicReceiver(closed.clone()),
        Interest::CONNECTION_LIFECYCLE,
    );
    let send = ConnectionObservationOwner::send(&provider).unwrap();
    let admitted = send.reserve_eligibility(true).unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| admitted.dispatch())).is_err()
    );
    send.closed(None);
    assert_eq!(closed.load(Ordering::SeqCst), 1);

    let connection = ConnectionAssociation::allocate();
    LocalConnectionObservationOwner::local(&provider)
        .unwrap()
        .register(connection);
    let admitted =
        LocalConnectionObservationOwner::reserve_registered_eligibility(connection, true).unwrap();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| admitted.dispatch())).is_err()
    );
    LocalConnectionObservationOwner::close_registered(connection, None);
    LocalConnectionObservationOwner::unregister(connection);
    assert_eq!(closed.load(Ordering::SeqCst), 2);
}
