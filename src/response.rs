use bytes::Bytes;
use http::header::{CONTENT_LENGTH, HeaderMap};
use http::{StatusCode, Version};
use http_body_util::BodyExt;

use crate::error::{Error, HyperBody, Result};

pub struct Response {
    inner: http::Response<HyperBody>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.inner.status())
            .field("version", &self.inner.version())
            .finish_non_exhaustive()
    }
}

impl Response {
    pub(crate) fn new(inner: http::Response<HyperBody>) -> Self {
        Self { inner }
    }

    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    pub fn version(&self) -> Version {
        self.inner.version()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.inner
            .headers()
            .get(CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .parse()
            .ok()
    }

    pub async fn bytes(self) -> Result<Bytes> {
        let body = self.inner.into_body();
        let collected = body
            .collect()
            .await
            .map_err(|e| Error::Other(Box::new(e)))?;
        Ok(collected.to_bytes())
    }

    pub async fn text(self) -> Result<String> {
        let bytes = self.bytes().await?;
        String::from_utf8(bytes.to_vec()).map_err(|e| Error::Other(Box::new(e)))
    }

    #[cfg(feature = "json")]
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Other(Box::new(e)))
    }

    pub fn into_body(self) -> HyperBody {
        self.inner.into_body()
    }

    pub fn into_sse_stream(self) -> crate::sse::SseStream {
        crate::sse::SseStream::new(self.inner.into_body())
    }
}
