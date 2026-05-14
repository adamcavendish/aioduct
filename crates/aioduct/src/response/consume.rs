use bytes::Bytes;
#[cfg(feature = "json")]
use http::header::CONTENT_TYPE;
use http_body_util::BodyExt;

use crate::body::RequestBoxBody;
use crate::error::Error;
use crate::observer::{self, RequestEvent, RequestPhase, TransferDirection};

use super::Response;

impl Response {
    /// Consume the response body and return it as bytes.
    pub async fn bytes(self) -> Result<Bytes, Error> {
        let observer_ctx = self.observer_ctx;
        let response_started = observer_ctx.as_ref().map(|c| c.response_started);
        let body = self.inner.into_body();
        match body.collect().await {
            Ok(collected) => {
                let bytes = collected.to_bytes();
                if let Some(ctx) = &observer_ctx {
                    let total_bytes = bytes.len() as u64;
                    let transfer_duration = ctx.response_started.elapsed();
                    let throughput = if transfer_duration.as_secs_f64() > 0.0 {
                        (total_bytes as f64 / transfer_duration.as_secs_f64()) as f32
                    } else {
                        0.0
                    };
                    ctx.observer.on_event(&RequestEvent {
                        method: ctx.method.clone(),
                        uri: ctx.uri.clone(),
                        phase: RequestPhase::TransferComplete {
                            direction: TransferDirection::Download,
                            total_bytes,
                            transfer_duration,
                            throughput_bytes_per_sec: throughput,
                        },
                        at: observer::Instant::now(),
                    });
                }
                Ok(bytes)
            }
            Err(e) => {
                if let Some(ctx) = &observer_ctx {
                    ctx.observer.on_event(&RequestEvent {
                        method: ctx.method.clone(),
                        uri: ctx.uri.clone(),
                        phase: RequestPhase::TransferAborted {
                            direction: TransferDirection::Download,
                            bytes_transferred: 0,
                            elapsed: response_started.map(|t| t.elapsed()).unwrap_or_default(),
                            error: e.to_string(),
                        },
                        at: observer::Instant::now(),
                    });
                }
                Err(e)
            }
        }
    }

    /// Consume the response body and return it as a UTF-8 string.
    pub async fn text(self) -> Result<String, Error> {
        #[cfg(feature = "charset")]
        {
            self.text_with_charset("utf-8").await
        }
        #[cfg(not(feature = "charset"))]
        {
            let bytes = self.bytes().await?;
            String::from_utf8(bytes.to_vec()).map_err(|e| Error::Other(Box::new(e)))
        }
    }

    #[cfg(feature = "charset")]
    /// Consume the response body and decode it using the charset from Content-Type,
    /// falling back to the given default encoding.
    pub async fn text_with_charset(self, default_encoding: &str) -> Result<String, Error> {
        let content_type = self
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<mime::Mime>().ok());
        let encoding_name = content_type
            .as_ref()
            .and_then(|mime| mime.get_param("charset"))
            .map(|charset| charset.as_str())
            .unwrap_or(default_encoding);
        let encoding = encoding_rs::Encoding::for_label(encoding_name.as_bytes())
            .unwrap_or(encoding_rs::UTF_8);
        let bytes = self.bytes().await?;
        let (text, _, _) = encoding.decode(&bytes);
        Ok(text.into_owned())
    }

    /// Consume the response body and deserialize it as JSON.
    #[cfg(feature = "json")]
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Other(Box::new(e)))
    }

    /// Consume the response body and deserialize it as RFC 9457 Problem Details.
    ///
    /// Checks that the `Content-Type` is `application/problem+json` before
    /// attempting to parse. Returns `None` if the content type does not match.
    #[cfg(feature = "json")]
    pub async fn problem_details(self) -> Option<Result<crate::problem::ProblemDetails, Error>> {
        let is_problem = self
            .inner
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| {
                let ct = ct.to_lowercase();
                ct.starts_with("application/problem+json")
            })
            .unwrap_or(false);
        if !is_problem {
            return None;
        }
        Some(self.json().await)
    }

    /// Consume the response and return the raw hyper body.
    pub fn into_body(self) -> RequestBoxBody {
        self.inner.into_body().into_boxed()
    }

    /// Convert the response into an async byte stream.
    pub fn into_bytes_stream(self) -> crate::body::BodyStream {
        crate::body::BodyStream::with_observer(
            self.inner.into_body().into_boxed(),
            self.observer_ctx,
        )
    }

    /// Convert the response into a Server-Sent Events stream.
    pub fn into_sse_stream(self) -> crate::sse::SseStreamSend {
        crate::sse::SseStreamSend::new(self.inner.into_body().into_boxed())
    }

    /// Perform an HTTP upgrade (e.g., WebSocket) on this response.
    pub async fn upgrade(mut self) -> Result<crate::upgrade::Upgraded, Error> {
        crate::upgrade::on_upgrade(&mut self.inner).await
    }
}
