use crate::error::Error;

use super::Response;

// ── Conversion: ResponseBodySend → ResponseBodyLocal ───────────────────

impl Response {
    /// Convert to a `Response<ResponseBodyLocal>` for the Local runtime path.
    pub(crate) fn into_local(self) -> Response<crate::body::ResponseBodyLocal> {
        let (parts, body) = self.inner.into_parts();
        let local_body: crate::body::ResponseBodyLocal = Box::pin(body);
        Response {
            inner: http::Response::from_parts(parts, local_body),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            timings: self.timings,
            observer_ctx: self.observer_ctx,
        }
    }

    /// Convert to `Response<ResponseBodyLocal>`, wrapping the body with a read timeout.
    pub(crate) fn into_local_with_read_timeout<R: crate::runtime::RuntimeCompletion>(
        self,
        duration: std::time::Duration,
    ) -> Response<crate::body::ResponseBodyLocal> {
        let (parts, body) = self.inner.into_parts();
        let timeout_body = crate::timeout::ReadTimeoutBody::<_, R>::new(body, duration);
        let local_body: crate::body::ResponseBodyLocal = Box::pin(timeout_body);
        Response {
            inner: http::Response::from_parts(parts, local_body),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            timings: self.timings,
            observer_ctx: self.observer_ctx,
        }
    }
}

// ── Body consumption for Response<ResponseBodyLocal> ──────────────────────

impl Response<crate::body::ResponseBodyLocal> {
    /// Wrap the local body with a bandwidth limiter.
    pub(crate) fn apply_bandwidth_limit_local<R: crate::runtime::RuntimeCompletion>(
        self,
        limiter: crate::bandwidth::BandwidthLimiter,
    ) -> Self {
        let (parts, body) = self.inner.into_parts();
        let wrapped = crate::bandwidth::BandwidthBody::<_, R>::new(body, limiter);
        let local_body: crate::body::ResponseBodyLocal = Box::pin(wrapped);
        Self {
            inner: http::Response::from_parts(parts, local_body),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            timings: self.timings,
            observer_ctx: self.observer_ctx,
        }
    }

    /// Convert the response into a Server-Sent Events stream.
    pub fn into_sse_stream(self) -> crate::sse::SseStreamLocal {
        crate::sse::SseStreamLocal::new(self.inner.into_body())
    }

    /// Perform an HTTP upgrade (e.g., WebSocket) on this response.
    pub async fn upgrade(mut self) -> Result<crate::upgrade::UpgradedLocal, Error> {
        crate::upgrade::on_upgrade_local_manual(&mut self.inner).await
    }
}

#[cfg(all(test, feature = "compio"))]
mod tests {
    use crate::body::ResponseBodyLocal;
    use crate::response::Response;
    use crate::runtime::compio_rt::CompioRuntime;
    use http_body_util::BodyExt;

    fn make_local_response(body_bytes: &[u8]) -> Response<ResponseBodyLocal> {
        let body = http_body_util::Full::new(bytes::Bytes::from(body_bytes.to_vec()))
            .map_err(|never| match never {});
        let local_body: ResponseBodyLocal = Box::pin(body);
        let inner = http::Response::builder()
            .status(200)
            .body(local_body)
            .unwrap();
        Response {
            inner,
            url: "http://example.com/".parse().unwrap(),
            remote_addr: None,
            tls_info: None,
            timings: None,
            observer_ctx: None,
        }
    }

    #[test]
    fn bytes_local() {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resp = make_local_response(b"hello local");
            let bytes = resp.bytes().await.unwrap();
            assert_eq!(bytes, "hello local");
        });
    }

    #[test]
    fn text_local() {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resp = make_local_response(b"text body");
            let text = resp.text().await.unwrap();
            assert_eq!(text, "text body");
        });
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_local() {
        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resp = make_local_response(b"{\"key\":\"value\"}");
            let val: serde_json::Value = resp.json().await.unwrap();
            assert_eq!(val["key"], "value");
        });
    }

    #[test]
    fn into_local_conversion() {
        use crate::response::ResponseBodySend;
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"convert"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBodySend::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let local_resp = resp.into_local();
        assert_eq!(local_resp.status(), http::StatusCode::OK);
    }

    #[test]
    fn into_local_with_read_timeout() {
        use crate::response::ResponseBodySend;
        use std::time::Duration;
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"timeout"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBodySend::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let local_resp = resp.into_local_with_read_timeout::<CompioRuntime>(Duration::from_secs(5));
        assert_eq!(local_resp.status(), http::StatusCode::OK);
    }

    #[test]
    fn apply_bandwidth_limit_local() {
        use crate::bandwidth::BandwidthLimiter;
        let resp = make_local_response(b"bandwidth");
        let limited =
            resp.apply_bandwidth_limit_local::<CompioRuntime>(BandwidthLimiter::new(1024));
        assert_eq!(limited.status(), http::StatusCode::OK);
    }
}
