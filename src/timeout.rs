use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

pin_project! {
    #[project = TimeoutProj]
    pub enum Timeout<F, S> {
        NoTimeout { #[pin] future: F },
        WithTimeout { #[pin] future: F, #[pin] sleep: S },
    }
}

impl<F, S, T, E> Future for Timeout<F, S>
where
    F: Future<Output = Result<T, E>>,
    S: Future<Output = ()>,
    E: From<crate::error::Error>,
{
    type Output = Result<T, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            TimeoutProj::NoTimeout { future } => future.poll(cx),
            TimeoutProj::WithTimeout { future, sleep } => {
                if let Poll::Ready(result) = future.poll(cx) {
                    return Poll::Ready(result);
                }
                if let Poll::Ready(()) = sleep.poll(cx) {
                    return Poll::Ready(Err(crate::error::Error::Timeout.into()));
                }
                Poll::Pending
            }
        }
    }
}
