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
        inner: crate::error::AioductBody,
        duration: Duration,
        #[pin]
        sleep: Option<S::Sleep>,
    }
}

impl<S: crate::runtime::Runtime> ReadTimeoutBody<S> {
    pub fn new(inner: crate::error::AioductBody, duration: Duration) -> Self {
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

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use std::task::{Context, Poll};

    #[tokio::test]
    async fn no_timeout_passes_through() {
        let t: Timeout<_, std::future::Ready<()>> = Timeout::NoTimeout {
            future: async { Ok::<i32, crate::error::Error>(42) },
        };
        let result = t.await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn with_timeout_completes_before_deadline() {
        let t = Timeout::WithTimeout {
            future: async { Ok::<i32, crate::error::Error>(42) },
            sleep: tokio::time::sleep(Duration::from_secs(10)),
        };
        let result = t.await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn with_timeout_fires_on_slow_future() {
        let t = Timeout::WithTimeout {
            future: async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok::<i32, crate::error::Error>(42)
            },
            sleep: tokio::time::sleep(Duration::from_millis(10)),
        };
        let result = t.await;
        assert!(matches!(result, Err(crate::error::Error::Timeout)));
    }

    #[tokio::test]
    async fn read_timeout_body_end_stream() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::error::AioductBody = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed();
        let body = ReadTimeoutBody::<TokioRuntime>::new(inner, Duration::from_secs(1));
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn read_timeout_body_size_hint() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::error::AioductBody = http_body_util::Full::new(Bytes::from("hello"))
            .map_err(|never| match never {})
            .boxed();
        let body = ReadTimeoutBody::<TokioRuntime>::new(inner, Duration::from_secs(1));
        assert_eq!(body.size_hint().exact(), Some(5));
    }

    #[tokio::test]
    async fn read_timeout_body_passes_data() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::error::AioductBody = http_body_util::Full::new(Bytes::from("data"))
            .map_err(|never| match never {})
            .boxed();
        let body = ReadTimeoutBody::<TokioRuntime>::new(inner, Duration::from_secs(1));
        let mut boxed = Box::pin(body);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let frame = boxed.as_mut().poll_frame(&mut cx);
        match frame {
            Poll::Ready(Some(Ok(f))) => {
                let data = f.into_data().unwrap();
                assert_eq!(data, Bytes::from("data"));
            }
            other => panic!("expected data frame, got {:?}", other),
        }
    }
}
