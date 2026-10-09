use http_body_util::BodyExt;

use crate::body::RequestBodySend;

use super::{Response, ResponseBodySend};

impl Response {
    pub(crate) fn decompress(self, accept: &crate::decompress::AcceptEncoding) -> Self {
        let (mut parts, body) = self.inner.into_parts();
        let boxed = body.into_boxed();
        let boxed = crate::decompress::maybe_decompress(&mut parts.headers, boxed, accept);
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::from_boxed(boxed)),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            observer_ctx: self.observer_ctx,
            fragment: self.fragment,
        }
    }

    pub(crate) fn apply_read_timeout<R: crate::runtime::RuntimePoll>(
        self,
        duration: std::time::Duration,
    ) -> Self {
        let (parts, body) = self.inner.into_parts();
        let boxed = body.into_boxed();
        let timeout_body = crate::timeout::ReadTimeoutBody::<_, R>::new(boxed, duration);
        let boxed: RequestBodySend = timeout_body.map_err(|e| e).boxed_unsync();
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::from_boxed(boxed)),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            observer_ctx: self.observer_ctx,
            fragment: self.fragment,
        }
    }

    pub(crate) fn apply_bandwidth_limit<R: crate::runtime::RuntimePoll>(
        self,
        limiter: crate::bandwidth::BandwidthLimiter,
    ) -> Self {
        let (parts, body) = self.inner.into_parts();
        let boxed = body.into_boxed();
        let wrapped = crate::bandwidth::BandwidthBody::<_, R>::new(boxed, limiter);
        let boxed: RequestBodySend = wrapped.boxed_unsync();
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::from_boxed(boxed)),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            observer_ctx: self.observer_ctx,
            fragment: self.fragment,
        }
    }
}
