use bytes::Bytes;
use http_body_util::BodyExt;

use crate::error::Error;

use super::Response;

// ── Conversion: ResponseBoxSendBody → ResponseBoxLocalBody ───────────────────

impl Response {
    /// Convert to a `Response<ResponseBoxLocalBody>` for the Local runtime path.
    pub(crate) fn into_local(self) -> Response<crate::body::ResponseBoxLocalBody> {
        let (parts, body) = self.inner.into_parts();
        let local_body: crate::body::ResponseBoxLocalBody = Box::pin(body);
        Response {
            inner: http::Response::from_parts(parts, local_body),
            url: self.url,
            remote_addr: self.remote_addr,
            tls_info: self.tls_info,
            timings: self.timings,
            observer_ctx: self.observer_ctx,
        }
    }

    /// Convert to `Response<ResponseBoxLocalBody>`, wrapping the body with a read timeout.
    pub(crate) fn into_local_with_read_timeout<R: crate::runtime::RuntimeCompletion>(
        self,
        duration: std::time::Duration,
    ) -> Response<crate::body::ResponseBoxLocalBody> {
        let (parts, body) = self.inner.into_parts();
        let timeout_body = crate::timeout::ReadTimeoutResponseBody::<R>::new(body, duration);
        let local_body: crate::body::ResponseBoxLocalBody = Box::pin(timeout_body);
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

// ── Body consumption for Response<ResponseBoxLocalBody> ──────────────────────

impl Response<crate::body::ResponseBoxLocalBody> {
    /// Consume the response body and return it as bytes.
    pub async fn bytes(self) -> Result<Bytes, Error> {
        let body = self.inner.into_body();
        let collected = body.collect().await?;
        Ok(collected.to_bytes())
    }

    /// Consume the response body and return it as a UTF-8 string.
    pub async fn text(self) -> Result<String, Error> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}
