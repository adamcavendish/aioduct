use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame};
use pin_project_lite::pin_project;

use crate::error::Error;

use super::BandwidthLimiter;

pin_project! {
    /// Body wrapper that enforces bandwidth limits on response data.
    ///
    /// Wraps `ResponseBoxLocalBody` and is boxed via `Box::pin()` so `S::Sleep`
    /// need not be `Send`. Used by the Local path (compio).
    pub(crate) struct BandwidthResponseBody<S: crate::runtime::RuntimeCompletion> {
        #[pin]
        inner: crate::body::ResponseBoxLocalBody,
        limiter: BandwidthLimiter,
        pending: Option<Bytes>,
        #[pin]
        sleep: Option<S::Sleep>,
    }
}

impl<S: crate::runtime::RuntimeCompletion> BandwidthResponseBody<S> {
    pub(crate) fn new(inner: crate::body::ResponseBoxLocalBody, limiter: BandwidthLimiter) -> Self {
        Self {
            inner,
            limiter,
            pending: None,
            sleep: None,
        }
    }
}

impl<S: crate::runtime::RuntimeCompletion> Body for BandwidthResponseBody<S> {
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
