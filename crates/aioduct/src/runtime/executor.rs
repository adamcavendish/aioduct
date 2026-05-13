use std::future::Future;
use std::marker::PhantomData;

use super::{RuntimeLocal, RuntimePoll};

/// Send executor for hyper's HTTP/2 handshake — spawns onto a work-stealing pool.
pub(crate) struct PollExecutor<R>(PhantomData<fn() -> R>);

impl<R> Clone for PollExecutor<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for PollExecutor<R> {}

impl<R, F> hyper::rt::Executor<F> for PollExecutor<R>
where
    R: RuntimePoll,
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        R::spawn_send(fut);
    }
}

/// Create a [`PollExecutor`] for the given runtime.
pub(crate) fn poll_executor<R: RuntimePoll>() -> PollExecutor<R> {
    PollExecutor(PhantomData)
}

/// `!Send` executor for hyper's HTTP/2 handshake — spawns on the current
/// thread's event loop. Used by completion-based runtimes (compio).
pub(crate) struct CompletionExecutor<R>(PhantomData<fn() -> R>);

impl<R> Clone for CompletionExecutor<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for CompletionExecutor<R> {}

impl<R, F> hyper::rt::Executor<F> for CompletionExecutor<R>
where
    R: RuntimeLocal,
    F: Future<Output = ()> + 'static,
{
    fn execute(&self, fut: F) {
        R::spawn_local(fut);
    }
}

/// Create a [`CompletionExecutor`] for the given runtime.
pub(crate) fn completion_executor<R: RuntimeLocal>() -> CompletionExecutor<R> {
    CompletionExecutor(PhantomData)
}
