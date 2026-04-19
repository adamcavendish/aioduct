use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body::Frame;
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

pin_project! {
    /// Body wrapper that enforces a timeout between data chunks.
    pub struct ReadTimeoutBody<S: crate::runtime::Runtime> {
        #[pin]
        inner: crate::error::HyperBody,
        duration: Duration,
        #[pin]
        sleep: Option<S::Sleep>,
    }
}

impl<S: crate::runtime::Runtime> ReadTimeoutBody<S> {
    pub fn new(inner: crate::error::HyperBody, duration: Duration) -> Self {
        Self {
            inner,
            duration,
            sleep: None,
        }
    }
}

impl<S: crate::runtime::Runtime> http_body::Body for ReadTimeoutBody<S> {
    type Data = Bytes;
    type Error = crate::error::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();

        match this.inner.poll_frame(cx) {
            Poll::Ready(result) => {
                this.sleep.set(None);
                Poll::Ready(result)
            }
            Poll::Pending => {
                if this.sleep.as_ref().get_ref().is_none() {
                    this.sleep.set(Some(S::sleep(*this.duration)));
                }
                if let Some(sleep) = this.sleep.as_mut().as_pin_mut() {
                    if let Poll::Ready(()) = sleep.poll(cx) {
                        this.sleep.set(None);
                        return Poll::Ready(Some(Err(crate::error::Error::Timeout)));
                    }
                }
                Poll::Pending
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
