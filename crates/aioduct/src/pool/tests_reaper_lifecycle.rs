use super::*;
use crate::body::{RequestBodyLocal, RequestBodySend};
use crate::runtime::{RuntimeCompletion, RuntimeLocal};
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

thread_local! {
    static REAPER: RefCell<Option<Pin<Box<dyn Future<Output = ()>>>>> = const { RefCell::new(None) };
}

struct TestRuntime;

// Suspend at each sleep so ownership and task completion can be checked
// deterministically, without waiting for the configured idle timeout.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

impl RuntimeCompletion for TestRuntime {
    type Sleep = YieldOnce;

    fn sleep(_duration: Duration) -> Self::Sleep {
        YieldOnce(false)
    }

    fn block_on<F: Future>(_future: F) -> Result<F::Output, crate::Error> {
        unreachable!("tests poll the reaper directly")
    }
}

impl RuntimeLocal for TestRuntime {
    fn spawn_local<F: Future<Output = ()> + 'static>(future: F) {
        REAPER.with(|slot| {
            assert!(slot.borrow_mut().replace(Box::pin(future)).is_none());
        });
    }
}

impl RuntimePoll for TestRuntime {
    fn spawn_send<F: Future<Output = ()> + Send + 'static>(future: F) {
        Self::spawn_local(future);
    }
}

fn assert_reaper_lifecycle<B: 'static>(
    pool: ConnectionPool<B>,
    start: impl Fn(&ConnectionPool<B>),
    drop_before_poll: bool,
) {
    let inner = Arc::downgrade(&pool.inner);
    let counters = Arc::downgrade(&pool.counters);
    start(&pool);
    let mut task = REAPER.with(|slot| slot.borrow_mut().take().unwrap());
    let mut cx = Context::from_waker(Waker::noop());

    if !drop_before_poll {
        let clone = pool.clone();
        start(&clone);
        assert!(task.as_mut().poll(&mut cx).is_pending());
        drop(pool);
        assert!(
            inner.upgrade().is_some(),
            "a client clone still owns the pool"
        );
        assert!(task.as_mut().poll(&mut cx).is_pending());
        drop(clone);
    } else {
        drop(pool);
    }

    assert!(
        inner.upgrade().is_none(),
        "the reaper must not retain the pool"
    );
    assert!(
        task.as_mut().poll(&mut cx).is_ready(),
        "the reaper must stop after pool drop"
    );
    assert!(
        counters.upgrade().is_none(),
        "the stopped reaper must release its counters"
    );
}

#[test]
fn send_reaper_releases_pool_while_sleeping() {
    assert_reaper_lifecycle(
        ConnectionPool::<RequestBodySend>::new(),
        ConnectionPool::ensure_reaper::<TestRuntime>,
        false,
    );
}

#[test]
fn local_reaper_releases_pool_while_sleeping() {
    assert_reaper_lifecycle(
        ConnectionPool::<RequestBodyLocal>::new(),
        ConnectionPool::ensure_reaper_local::<TestRuntime>,
        false,
    );
}

#[test]
fn send_reaper_stops_if_pool_dropped_before_start() {
    assert_reaper_lifecycle(
        ConnectionPool::<RequestBodySend>::new(),
        ConnectionPool::ensure_reaper::<TestRuntime>,
        true,
    );
}

#[test]
fn local_reaper_stops_if_pool_dropped_before_start() {
    assert_reaper_lifecycle(
        ConnectionPool::<RequestBodyLocal>::new(),
        ConnectionPool::ensure_reaper_local::<TestRuntime>,
        true,
    );
}
