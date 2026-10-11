use http_body_util::BodyExt;

use crate::body::RequestBodySend;

use super::{Response, ResponseBodySend};

impl Response {
    pub(crate) fn decompress(self, accept: &crate::decompress::AcceptEncoding) -> Self {
        let (mut parts, body) = self.inner.into_parts();
        let (boxed, owner) = body.into_parts();
        let boxed = crate::decompress::maybe_decompress(&mut parts.headers, boxed, accept);
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::with_owner(boxed, owner)),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            fragment: self.fragment,
        }
    }

    pub(crate) fn apply_read_timeout<R: crate::runtime::RuntimePoll>(
        self,
        duration: std::time::Duration,
    ) -> Self {
        let (parts, body) = self.inner.into_parts();
        let (boxed, owner) = body.into_parts();
        let timeout_body = crate::timeout::ReadTimeoutBody::<_, R>::new(boxed, duration);
        let boxed: RequestBodySend = timeout_body.map_err(|e| e).boxed_unsync();
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::with_owner(boxed, owner)),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            fragment: self.fragment,
        }
    }

    pub(crate) fn apply_bandwidth_limit<R: crate::runtime::RuntimePoll>(
        self,
        limiter: crate::bandwidth::BandwidthLimiter,
    ) -> Self {
        let (parts, body) = self.inner.into_parts();
        let (boxed, owner) = body.into_parts();
        let wrapped = crate::bandwidth::BandwidthBody::<_, R>::new(boxed, limiter);
        let boxed: RequestBodySend = wrapped.boxed_unsync();
        Self {
            inner: http::Response::from_parts(parts, ResponseBodySend::with_owner(boxed, owner)),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            fragment: self.fragment,
        }
    }
}
