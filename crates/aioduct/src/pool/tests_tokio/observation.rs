use super::*;
use crate::observer::{
    ConnectionObservation, ConnectionObservationEvent, Interest, NoopRequestObservation, Observer,
    RequestInfo,
};

#[derive(Clone)]
struct Recorder {
    inner: Weak<Mutex<PoolInner<RequestBodySend>>>,
    facts: Arc<Mutex<Vec<(bool, u64)>>>,
}
impl ConnectionObservation for Recorder {
    fn on_connection(&self, event: &ConnectionObservationEvent<'_>) {
        if let ConnectionObservationEvent::PoolEligibilityChanged {
            reusable, revision, ..
        } = event
        {
            let inner = self.inner.upgrade().unwrap();
            assert!(
                inner.try_lock().is_ok(),
                "user callback must run outside pool lock"
            );
            self.facts.lock().unwrap().push((*reusable, *revision));
        }
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            assert!(
                inner.try_lock().is_ok(),
                "user observation Drop must run outside pool lock"
            );
        }
    }
}
struct Provider(Recorder);
impl Observer for Provider {
    type Context = ();
    type Observation = NoopRequestObservation;
    type ConnectionObservation = Recorder;
    fn begin(&self, _: RequestInfo<'_>, _: Option<()>) -> (Interest, Self::Observation) {
        (Interest::EMPTY, NoopRequestObservation)
    }
    fn begin_connection(&self) -> (Interest, Self::ConnectionObservation) {
        (Interest::CONNECTION_LIFECYCLE, self.0.clone())
    }
}

async fn observed(
    pool: &ConnectionPool<RequestBodySend>,
    facts: Arc<Mutex<Vec<(bool, u64)>>>,
) -> PooledConnection<RequestBodySend> {
    let mut connection = make_h1_conn().await;
    let provider = Provider(Recorder {
        inner: Arc::downgrade(&pool.inner),
        facts,
    });
    connection.metrics = Arc::new(super::super::connection::ConnectionMetrics::new(
        crate::observer::ConnectionObservationOwner::send(&provider),
    ));
    connection.observe_established();
    connection
}

#[tokio::test]
async fn reuse_is_not_checkin_or_checkout_and_eviction_is_not_close() {
    let pool = ConnectionPool::new().without_reaper();
    let facts = Arc::new(Mutex::new(Vec::new()));
    let mut connection = observed(&pool, facts.clone()).await;
    assert!(std::future::poll_fn(|cx| connection.poll_ready(cx)).await);
    let k = key("example.com:80");
    pool.checkin(k.clone(), connection);
    let connection = pool.checkout(&k).unwrap();
    pool.checkin(k.clone(), connection);
    assert_eq!(*facts.lock().unwrap(), [(true, 1)]);
    pool.evict(&k);
    assert_eq!(*facts.lock().unwrap(), [(true, 1), (false, 2)]);
}

#[tokio::test]
async fn capacity_checkout_expiry_and_reaper_release_user_objects_outside_lock() {
    for path in 0..3 {
        let pool = ConnectionPool::new()
            .without_reaper()
            .with_max_idle_per_host(1);
        let facts = Arc::new(Mutex::new(Vec::new()));
        let k = key("example.com:80");
        let connection = observed(&pool, facts.clone()).await;
        pool.checkin(k.clone(), connection);
        match path {
            0 => {
                pool.checkin(k, make_h1_conn().await);
            }
            1 => {
                pool.inner.lock().unwrap().idle_timeout = Duration::ZERO;
                assert!(pool.checkout(&k).is_none());
            }
            _ => {
                pool.inner.lock().unwrap().idle_timeout = Duration::ZERO;
                let mut retired = Vec::new();
                let mut inner = pool.inner.lock().unwrap();
                ConnectionPool::prune_expired(&mut inner, &pool.counters, &mut retired);
                drop(inner);
                for (_entry, change) in retired {
                    if let Some(change) = change {
                        change.dispatch();
                    }
                }
            }
        }
        assert_eq!(*facts.lock().unwrap(), [(true, 1), (false, 2)]);
    }
}

#[tokio::test]
async fn connection_which_was_never_reusable_has_no_eligibility_change() {
    let pool = ConnectionPool::new()
        .without_reaper()
        .with_max_idle_per_host(0);
    let facts = Arc::new(Mutex::new(Vec::new()));
    let connection = observed(&pool, facts.clone()).await;
    pool.checkin(key("example.com:80"), connection);
    assert!(facts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn key_wide_retirement_records_each_distinct_removed_transport() {
    let pool = ConnectionPool::new().without_reaper();
    let k = key("example.com:80");
    let a = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::new(Mutex::new(Vec::new()));
    for facts in [a.clone(), b.clone()] {
        let mut connection = super::make_h2_conn().await;
        let provider = Provider(Recorder {
            inner: Arc::downgrade(&pool.inner),
            facts,
        });
        connection.metrics = Arc::new(super::super::connection::ConnectionMetrics::new(
            crate::observer::ConnectionObservationOwner::send(&provider),
        ));
        connection.observe_established();
        pool.checkin(k.clone(), connection);
    }
    let mut acquired = pool.checkout(&k).unwrap();
    acquired.retire_multiplex_transport();
    assert_eq!(*a.lock().unwrap(), [(true, 1), (false, 2)]);
    assert_eq!(*b.lock().unwrap(), [(true, 1), (false, 2)]);
    assert!(pool.checkout(&k).is_none());
}
