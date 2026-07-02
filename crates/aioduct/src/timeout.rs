use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body::Body;
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
    /// Body wrapper that records when request upload has ended.
    pub(crate) struct BodyCompletion<B> {
        #[pin]
        inner: B,
        complete: Arc<AtomicBool>,
    }
}

pub(crate) fn mark_body_completion<B>(body: B) -> (BodyCompletion<B>, Arc<AtomicBool>)
where
    B: Body,
{
    let complete = Arc::new(AtomicBool::new(body.is_end_stream()));
    (
        BodyCompletion {
            inner: body,
            complete: complete.clone(),
        },
        complete,
    )
}

impl<B> Body for BodyCompletion<B>
where
    B: Body<Data = Bytes, Error = crate::error::Error>,
{
    type Data = Bytes;
    type Error = crate::error::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(None) => {
                this.complete.store(true, Ordering::Release);
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                this.complete.store(true, Ordering::Release);
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(Some(Ok(frame))) => {
                if this.inner.is_end_stream() {
                    this.complete.store(true, Ordering::Release);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

pin_project! {
    /// Timeout that starts after request upload completes.
    pub(crate) struct FirstByteTimeout<F, R>
    where
        R: crate::runtime::RuntimeCompletion,
    {
        #[pin]
        future: F,
        request_body_complete: Arc<AtomicBool>,
        duration: Duration,
        #[pin]
        sleep: Option<R::Sleep>,
        _runtime: PhantomData<R>,
    }
}

impl<F, R> FirstByteTimeout<F, R>
where
    R: crate::runtime::RuntimeCompletion,
{
    pub(crate) fn new(
        future: F,
        request_body_complete: Arc<AtomicBool>,
        duration: Duration,
    ) -> Self {
        Self {
            future,
            request_body_complete,
            duration,
            sleep: None,
            _runtime: PhantomData,
        }
    }
}

impl<F, R, T, E> Future for FirstByteTimeout<F, R>
where
    F: Future<Output = Result<T, E>>,
    R: crate::runtime::RuntimeCompletion,
    E: From<crate::error::Error>,
{
    type Output = Result<T, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if let Poll::Ready(result) = this.future.as_mut().poll(cx) {
            return Poll::Ready(result);
        }

        if this.request_body_complete.load(Ordering::Acquire) {
            if this.sleep.as_ref().get_ref().is_none() {
                this.sleep.set(Some(R::sleep(*this.duration)));
            }
            if let Some(sleep) = this.sleep.as_mut().as_pin_mut()
                && let Poll::Ready(()) = sleep.poll(cx)
            {
                this.sleep.set(None);
                return Poll::Ready(Err(crate::error::Error::Timeout.into()));
            }
        }

        Poll::Pending
    }
}

/// Race a future against an optional connect timeout using the runtime's sleep.
///
/// Maps `Error::Timeout` to `Error::ConnectTimeout` so proxy handshake timeouts
/// are classified correctly by `is_connect()`.
pub(crate) async fn connect_timeout<R, F, T>(
    future: F,
    timeout: Option<Duration>,
) -> Result<T, crate::error::Error>
where
    R: crate::runtime::RuntimeCompletion,
    F: Future<Output = Result<T, crate::error::Error>>,
{
    match timeout {
        Some(duration) => {
            match (Timeout::WithTimeout {
                future,
                sleep: R::sleep(duration),
            })
            .await
            {
                Err(crate::error::Error::Timeout) => Err(crate::error::Error::ConnectTimeout),
                other => other,
            }
        }
        None => future.await,
    }
}

pin_project! {
    /// Body wrapper that enforces a timeout between data chunks.
    ///
    /// Generic over the inner body type `B` — works with both `RequestBodySend`
    /// (Send path) and `ResponseBodyLocal` (Local path).
    pub(crate) struct ReadTimeoutBody<B, S: crate::runtime::RuntimeCompletion> {
        #[pin]
        inner: B,
        duration: Duration,
        #[pin]
        sleep: Option<S::Sleep>,
    }
}

impl<B, S: crate::runtime::RuntimeCompletion> ReadTimeoutBody<B, S> {
    pub fn new(inner: B, duration: Duration) -> Self {
        Self {
            inner,
            duration,
            sleep: None,
        }
    }
}

impl<B, S> http_body::Body for ReadTimeoutBody<B, S>
where
    B: http_body::Body<Data = Bytes, Error = crate::error::Error>,
    S: crate::runtime::RuntimeCompletion,
{
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
                if let Some(sleep) = this.sleep.as_mut().as_pin_mut()
                    && let Poll::Ready(()) = sleep.poll(cx)
                {
                    this.sleep.set(None);
                    return Poll::Ready(Some(Err(crate::error::Error::ReadTimeout)));
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

pin_project! {
    /// Body wrapper that enforces a timeout between data chunks during upload.
    ///
    /// Generic over the inner body type `B` — works with both `RequestBodySend`
    /// (Send path) and `RequestBodyLocal` (Local path).
    ///
    /// When the HTTP engine cannot accept more data (flow-control backpressure),
    /// `poll_frame` returns `Pending` and the sleep timer starts. If the inner
    /// body remains `Pending` beyond the configured duration, an
    /// [`Error::WriteTimeout`](crate::error::Error::WriteTimeout) is emitted.
    pub(crate) struct WriteTimeoutBody<B, S: crate::runtime::RuntimeCompletion> {
        #[pin]
        inner: B,
        duration: Duration,
        #[pin]
        sleep: Option<S::Sleep>,
    }
}

impl<B, S: crate::runtime::RuntimeCompletion> WriteTimeoutBody<B, S> {
    pub fn new(inner: B, duration: Duration) -> Self {
        Self {
            inner,
            duration,
            sleep: None,
        }
    }
}

impl<B, S> http_body::Body for WriteTimeoutBody<B, S>
where
    B: http_body::Body<Data = Bytes, Error = crate::error::Error>,
    S: crate::runtime::RuntimeCompletion,
{
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
                if let Some(sleep) = this.sleep.as_mut().as_pin_mut()
                    && let Poll::Ready(()) = sleep.poll(cx)
                {
                    this.sleep.set(None);
                    return Poll::Ready(Some(Err(crate::error::Error::WriteTimeout)));
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
    async fn connect_timeout_maps_timeout_classification() {
        use crate::runtime::tokio_rt::TokioRuntime;

        let result = connect_timeout::<TokioRuntime, _, i32>(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(42)
            },
            Some(Duration::from_millis(1)),
        )
        .await;

        assert!(matches!(result, Err(crate::error::Error::ConnectTimeout)));
    }

    #[tokio::test]
    async fn read_timeout_body_end_stream() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::body::RequestBodySend = http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed_unsync();
        let body = ReadTimeoutBody::<_, TokioRuntime>::new(inner, Duration::from_secs(1));
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn read_timeout_body_size_hint() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::body::RequestBodySend = http_body_util::Full::new(Bytes::from("hello"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let body = ReadTimeoutBody::<_, TokioRuntime>::new(inner, Duration::from_secs(1));
        assert_eq!(body.size_hint().exact(), Some(5));
    }

    #[tokio::test]
    async fn read_timeout_body_passes_data() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::body::RequestBodySend = http_body_util::Full::new(Bytes::from("data"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let body = ReadTimeoutBody::<_, TokioRuntime>::new(inner, Duration::from_secs(1));
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

    #[tokio::test]
    async fn read_timeout_body_fires_on_pending() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;

        struct PendingBody;

        impl http_body::Body for PendingBody {
            type Data = Bytes;
            type Error = crate::error::Error;

            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                Poll::Pending
            }

            fn is_end_stream(&self) -> bool {
                false
            }
        }

        use http_body_util::BodyExt;
        let inner: crate::body::RequestBodySend = PendingBody.boxed_unsync();
        let body = ReadTimeoutBody::<_, TokioRuntime>::new(inner, Duration::from_millis(1));
        let mut boxed = Box::pin(body);

        tokio::time::sleep(Duration::from_millis(10)).await;

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = boxed.as_mut().poll_frame(&mut cx);

        tokio::time::sleep(Duration::from_millis(10)).await;
        let result = boxed.as_mut().poll_frame(&mut cx);
        assert!(
            matches!(
                result,
                Poll::Ready(Some(Err(crate::error::Error::ReadTimeout)))
            ),
            "expected ReadTimeout, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn read_timeout_with_response_body_send() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::body::RequestBodySend = http_body_util::Full::new(Bytes::from("data"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let resp_body = crate::response::ResponseBodySend::from_boxed(inner);
        let body = ReadTimeoutBody::<_, TokioRuntime>::new(resp_body, Duration::from_secs(1));
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

    #[tokio::test]
    async fn read_timeout_with_response_body_local_passes_data() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let local_body: crate::body::ResponseBodyLocal = Box::pin(
            http_body_util::Full::new(Bytes::from("local data")).map_err(|never| match never {}),
        );
        let body = ReadTimeoutBody::<_, TokioRuntime>::new(local_body, Duration::from_secs(1));
        let mut boxed = Box::pin(body);
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let frame = boxed.as_mut().poll_frame(&mut cx);
        match frame {
            Poll::Ready(Some(Ok(f))) => {
                let data = f.into_data().unwrap();
                assert_eq!(data, Bytes::from("local data"));
            }
            other => panic!("expected data frame, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_timeout_with_response_body_local_fires_on_pending() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;

        struct PendingLocalBody;
        impl http_body::Body for PendingLocalBody {
            type Data = Bytes;
            type Error = crate::error::Error;
            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                Poll::Pending
            }
            fn is_end_stream(&self) -> bool {
                false
            }
        }

        let local_body: crate::body::ResponseBodyLocal = Box::pin(PendingLocalBody);
        let body = ReadTimeoutBody::<_, TokioRuntime>::new(local_body, Duration::from_millis(1));
        let mut boxed = Box::pin(body);

        tokio::time::sleep(Duration::from_millis(10)).await;

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = boxed.as_mut().poll_frame(&mut cx);

        tokio::time::sleep(Duration::from_millis(10)).await;
        let result = boxed.as_mut().poll_frame(&mut cx);
        assert!(
            matches!(
                result,
                Poll::Ready(Some(Err(crate::error::Error::ReadTimeout)))
            ),
            "expected ReadTimeout on local body, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn write_timeout_body_passes_data() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;
        use http_body_util::BodyExt;

        let inner: crate::body::RequestBodySend = http_body_util::Full::new(Bytes::from("data"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let body = WriteTimeoutBody::<_, TokioRuntime>::new(inner, Duration::from_secs(1));
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

    #[tokio::test]
    async fn write_timeout_body_fires_on_pending() {
        use crate::runtime::tokio_rt::TokioRuntime;
        use http_body::Body;

        struct PendingBody;

        impl http_body::Body for PendingBody {
            type Data = Bytes;
            type Error = crate::error::Error;

            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                Poll::Pending
            }

            fn is_end_stream(&self) -> bool {
                false
            }
        }

        use http_body_util::BodyExt;
        let inner: crate::body::RequestBodySend = PendingBody.boxed_unsync();
        let body = WriteTimeoutBody::<_, TokioRuntime>::new(inner, Duration::from_millis(1));
        let mut boxed = Box::pin(body);

        tokio::time::sleep(Duration::from_millis(10)).await;

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = boxed.as_mut().poll_frame(&mut cx);

        tokio::time::sleep(Duration::from_millis(10)).await;
        let result = boxed.as_mut().poll_frame(&mut cx);
        assert!(
            matches!(
                result,
                Poll::Ready(Some(Err(crate::error::Error::WriteTimeout)))
            ),
            "expected WriteTimeout, got {:?}",
            result
        );
    }
}

/// Races a future against a deadline. Returns `Some(value)` if the future
/// completes first, or `None` if the deadline fires (timeout).
pub(crate) async fn race_deadline<F, S, T>(future: F, deadline: S) -> Option<T>
where
    F: Future<Output = T>,
    S: Future<Output = ()>,
{
    pin_project! {
        struct SelectLeft<F, S> {
            #[pin]
            left: F,
            #[pin]
            deadline: S,
        }
    }

    impl<F: Future<Output = T>, S: Future<Output = ()>, T> Future for SelectLeft<F, S> {
        type Output = Option<T>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let proj = self.project();
            if let Poll::Ready(val) = proj.left.poll(cx) {
                return Poll::Ready(Some(val));
            }
            if let Poll::Ready(()) = proj.deadline.poll(cx) {
                return Poll::Ready(None);
            }
            Poll::Pending
        }
    }

    SelectLeft {
        left: future,
        deadline,
    }
    .await
}
