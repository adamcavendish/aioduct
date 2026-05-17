use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame};
use pin_project_lite::pin_project;

use crate::error::Error;

use super::BandwidthLimiter;

pin_project! {
    /// Body wrapper that enforces bandwidth limits on data frames.
    ///
    /// Generic over the inner body type `B` — works with both `RequestBodySend`
    /// (Send path) and `ResponseBodyLocal` (Local path).
    pub(crate) struct BandwidthBody<B, S: crate::runtime::RuntimeCompletion> {
        #[pin]
        inner: B,
        limiter: BandwidthLimiter,
        pending: Option<Bytes>,
        #[pin]
        sleep: Option<S::Sleep>,
    }
}

impl<B, S: crate::runtime::RuntimeCompletion> BandwidthBody<B, S> {
    pub(crate) fn new(inner: B, limiter: BandwidthLimiter) -> Self {
        Self {
            inner,
            limiter,
            pending: None,
            sleep: None,
        }
    }
}

impl<B, S> Body for BandwidthBody<B, S>
where
    B: Body<Data = Bytes, Error = Error>,
    S: crate::runtime::RuntimeCompletion,
{
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();

        if let Some(sleep) = this.sleep.as_mut().as_pin_mut() {
            if sleep.poll(cx).is_pending() {
                return Poll::Pending;
            }
            this.sleep.set(None);
        }

        if let Some(data) = this.pending.as_ref() {
            let n = data.len() as u64;
            let wait = this.limiter.wait_duration(n);
            if wait.is_zero() {
                let _ = this.limiter.try_consume(n);
                if let Some(data) = this.pending.take() {
                    return Poll::Ready(Some(Ok(Frame::data(data))));
                }
            }
            this.sleep.set(Some(S::sleep(wait)));
            if let Some(sleep) = this.sleep.as_mut().as_pin_mut() {
                let _ = sleep.poll(cx);
            }
            return Poll::Pending;
        }

        match this.inner.poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                Ok(data) => {
                    let n = data.len() as u64;
                    if this.limiter.wait_duration(n).is_zero() {
                        let _ = this.limiter.try_consume(n);
                        Poll::Ready(Some(Ok(Frame::data(data))))
                    } else {
                        let wait = this.limiter.wait_duration(n);
                        *this.pending = Some(data);
                        this.sleep.set(Some(S::sleep(wait)));
                        if let Some(sleep) = this.sleep.as_mut().as_pin_mut() {
                            let _ = sleep.poll(cx);
                        }
                        Poll::Pending
                    }
                }
                Err(frame) => Poll::Ready(Some(Ok(frame))),
            },
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream() && self.pending.is_none()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use crate::body::RequestBodySend;
    use crate::runtime::tokio_rt::TokioRuntime;
    use http_body::Body;
    use http_body_util::BodyExt;
    use std::pin::Pin;
    use std::task::Context;

    struct OneChunkBody {
        data: Option<Bytes>,
    }

    impl Body for OneChunkBody {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            if let Some(data) = self.data.take() {
                Poll::Ready(Some(Ok(Frame::data(data))))
            } else {
                Poll::Ready(None)
            }
        }
        fn is_end_stream(&self) -> bool {
            self.data.is_none()
        }
        fn size_hint(&self) -> http_body::SizeHint {
            http_body::SizeHint::with_exact(self.data.as_ref().map(|d| d.len() as u64).unwrap_or(0))
        }
    }

    fn boxed_body(chunk: Bytes) -> RequestBodySend {
        OneChunkBody { data: Some(chunk) }.boxed_unsync()
    }

    fn local_body(chunk: Bytes) -> crate::body::ResponseBodyLocal {
        Box::pin(OneChunkBody { data: Some(chunk) })
    }

    #[test]
    fn send_body_passes_through_with_sufficient_tokens() {
        let body = boxed_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(1024);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        match result {
            Poll::Ready(Some(Ok(frame))) => {
                assert_eq!(frame.into_data().unwrap(), Bytes::from("hello"));
            }
            other => panic!("expected Ready(Ok(data)), got: {other:?}"),
        }
        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Ready(None)));
    }

    #[test]
    fn local_body_passes_through_with_sufficient_tokens() {
        let body = local_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(1024);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        match result {
            Poll::Ready(Some(Ok(frame))) => {
                assert_eq!(frame.into_data().unwrap(), Bytes::from("hello"));
            }
            other => panic!("expected Ready(Ok(data)), got: {other:?}"),
        }
        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Ready(None)));
    }

    #[tokio::test]
    async fn body_buffers_when_tokens_insufficient() {
        let body = boxed_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(1);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(
            matches!(result, Poll::Pending),
            "expected Pending, got: {result:?}"
        );
        assert!(!wrapped.is_end_stream());
    }

    #[test]
    fn body_passes_zero_length_frame() {
        let body = boxed_body(Bytes::new());
        let bw = BandwidthLimiter::new(1);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());
        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Ready(Some(Ok(_)))));
    }

    #[test]
    fn size_hint_delegates_to_inner() {
        let body = boxed_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(100);
        let wrapped = BandwidthBody::<_, TokioRuntime>::new(body, bw);
        assert_eq!(wrapped.size_hint().exact(), Some(5));
    }

    #[test]
    fn smoke_end_to_end_throttle() {
        let body = boxed_body(Bytes::from("ab"));
        let bw = BandwidthLimiter::new(10_000);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(
            matches!(result, Poll::Ready(Some(Ok(_)))),
            "got: {result:?}"
        );
    }

    #[test]
    fn non_data_frame_passes_through() {
        struct TrailerBody {
            sent_data: bool,
        }

        impl Body for TrailerBody {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                if !self.sent_data {
                    self.sent_data = true;
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("x-checksum", "abc123".parse().unwrap());
                    Poll::Ready(Some(Ok(Frame::trailers(trailers))))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let body: RequestBodySend = TrailerBody { sent_data: false }.boxed_unsync();
        let bw = BandwidthLimiter::new(1024);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        match result {
            Poll::Ready(Some(Ok(frame))) => {
                assert!(frame.is_trailers(), "expected trailers frame");
            }
            other => panic!("expected Ready(Ok(trailers)), got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn is_end_stream_false_when_pending_data() {
        let body = boxed_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(1);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Pending));
        assert!(!wrapped.is_end_stream());
    }

    #[test]
    fn error_from_inner_propagates() {
        struct ErrorBody;

        impl Body for ErrorBody {
            type Data = Bytes;
            type Error = Error;
            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                Poll::Ready(Some(Err(Error::Other("test error".into()))))
            }
        }

        let body: RequestBodySend = ErrorBody.boxed_unsync();
        let bw = BandwidthLimiter::new(1024);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Ready(Some(Err(_)))));
    }

    #[test]
    fn inner_pending_propagates() {
        struct PendingBody;

        impl Body for PendingBody {
            type Data = Bytes;
            type Error = Error;
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

        let body: RequestBodySend = PendingBody.boxed_unsync();
        let bw = BandwidthLimiter::new(1024);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Pending));
    }

    #[test]
    fn is_end_stream_true_when_inner_done_no_pending() {
        let body = boxed_body(Bytes::from("x"));
        let bw = BandwidthLimiter::new(1024);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let _ = wrapped.as_mut().poll_frame(&mut cx);
        assert!(wrapped.is_end_stream());
    }

    #[tokio::test]
    async fn pending_data_eventually_released_after_sleep() {
        let body = boxed_body(Bytes::from("hello"));
        let bw = BandwidthLimiter::new(100);
        bw.try_consume(100);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Pending));

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        let result = wrapped.as_mut().poll_frame(&mut cx);
        match result {
            Poll::Ready(Some(Ok(frame))) => {
                assert_eq!(frame.into_data().unwrap(), Bytes::from("hello"));
            }
            other => panic!("expected Ready(Ok(data)), got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_body_buffers_when_tokens_insufficient() {
        let body = local_body(Bytes::from("throttled"));
        let bw = BandwidthLimiter::new(1);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(
            matches!(result, Poll::Pending),
            "expected Pending for local body, got: {result:?}"
        );
        assert!(!wrapped.is_end_stream());
    }

    #[tokio::test]
    async fn local_body_pending_data_released_after_sleep() {
        let body = local_body(Bytes::from("delayed"));
        let bw = BandwidthLimiter::new(100);
        bw.try_consume(100);
        let mut wrapped = Box::pin(BandwidthBody::<_, TokioRuntime>::new(body, bw));

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);

        let result = wrapped.as_mut().poll_frame(&mut cx);
        assert!(matches!(result, Poll::Pending));

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        let result = wrapped.as_mut().poll_frame(&mut cx);
        match result {
            Poll::Ready(Some(Ok(frame))) => {
                assert_eq!(frame.into_data().unwrap(), Bytes::from("delayed"));
            }
            other => panic!("expected Ready(Ok(data)) for local body, got: {other:?}"),
        }
    }
}
