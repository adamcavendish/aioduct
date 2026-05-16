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

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use super::*;
    use crate::observer::{ConnectionEvent, RequestEvent, RequestObserver, RequestPhase};
    use crate::response::{BodyObserverCtx, Response, ResponseBoxSendBody};
    use std::sync::{Arc, Mutex};

    struct RecordingObserver {
        events: Arc<Mutex<Vec<RequestPhase>>>,
    }

    impl RequestObserver for RecordingObserver {
        fn on_event(&self, event: &RequestEvent) {
            self.events.lock().unwrap().push(event.phase.clone());
        }
        fn on_connection_event(&self, _event: &ConnectionEvent) {}
    }

    fn make_response_with_observer(
        body_bytes: &[u8],
        events: Arc<Mutex<Vec<RequestPhase>>>,
    ) -> Response {
        let body = http_body_util::Full::new(bytes::Bytes::from(body_bytes.to_vec()))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let mut resp = Response::new(inner, "http://example.com/".parse().unwrap());
        resp.set_observer_ctx(BodyObserverCtx {
            observer: Arc::new(RecordingObserver {
                events: events.clone(),
            }),
            method: http::Method::GET,
            uri: "http://example.com/".parse().unwrap(),
            response_started: crate::clock::Instant::now(),
        });
        resp
    }

    #[tokio::test]
    async fn bytes_success_fires_transfer_complete() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let resp = make_response_with_observer(b"hello world", events.clone());
        let bytes = resp.bytes().await.unwrap();
        assert_eq!(bytes, "hello world");
        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(matches!(captured[0], RequestPhase::TransferComplete { .. }));
    }

    #[tokio::test]
    async fn bytes_success_without_observer() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"no observer"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let bytes = resp.bytes().await.unwrap();
        assert_eq!(bytes, "no observer");
    }

    #[tokio::test]
    async fn bytes_error_fires_transfer_aborted() {
        use http_body::Body;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        struct ErrorBody;

        impl Body for ErrorBody {
            type Data = bytes::Bytes;
            type Error = Error;

            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                Poll::Ready(Some(Err(Error::Other("test error".into()))))
            }
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let boxed: crate::body::RequestBoxBody = http_body_util::BodyExt::boxed_unsync(ErrorBody);
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBoxSendBody::from_boxed(boxed))
            .unwrap();
        let mut resp = Response::new(inner, "http://example.com/".parse().unwrap());
        resp.set_observer_ctx(BodyObserverCtx {
            observer: Arc::new(RecordingObserver {
                events: events.clone(),
            }),
            method: http::Method::POST,
            uri: "http://example.com/upload".parse().unwrap(),
            response_started: crate::clock::Instant::now(),
        });

        let result = resp.bytes().await;
        assert!(result.is_err());

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(matches!(captured[0], RequestPhase::TransferAborted { .. }));
    }

    #[tokio::test]
    async fn text_returns_utf8_string() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"hello text"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let text = resp.text().await.unwrap();
        assert_eq!(text, "hello text");
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn json_deserializes() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"{\"key\":\"value\"}"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let val: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(val["key"], "value");
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn problem_details_wrong_content_type_returns_none() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"{}"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(400)
            .header("content-type", "application/json")
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let result = resp.problem_details().await;
        assert!(result.is_none());
    }

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn problem_details_correct_content_type() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(
            b"{\"type\":\"about:blank\",\"title\":\"Not Found\",\"status\":404}",
        ))
        .map_err(|never| match never {})
        .boxed_unsync();
        let inner = http::Response::builder()
            .status(404)
            .header("content-type", "application/problem+json")
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let result = resp.problem_details().await;
        assert!(result.is_some());
        let pd = result.unwrap().unwrap();
        assert_eq!(pd.title.as_deref(), Some("Not Found"));
    }

    #[cfg(feature = "charset")]
    #[tokio::test]
    async fn text_with_charset_respects_content_type_charset() {
        // Latin-1 encoded: "caf\xe9"
        let body = http_body_util::Full::new(bytes::Bytes::from(vec![0x63, 0x61, 0x66, 0xe9]))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .header("content-type", "text/plain; charset=iso-8859-1")
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let text = resp.text_with_charset("utf-8").await.unwrap();
        assert_eq!(text, "caf\u{e9}");
    }

    #[cfg(feature = "charset")]
    #[tokio::test]
    async fn text_with_charset_uses_default_when_no_charset_param() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"plain text"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .header("content-type", "text/plain")
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let text = resp.text_with_charset("utf-8").await.unwrap();
        assert_eq!(text, "plain text");
    }

    #[cfg(feature = "charset")]
    #[tokio::test]
    async fn text_with_charset_uses_default_when_no_content_type() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"no ct"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let text = resp.text_with_charset("utf-8").await.unwrap();
        assert_eq!(text, "no ct");
    }

    #[cfg(feature = "charset")]
    #[tokio::test]
    async fn text_with_charset_unknown_encoding_falls_back_to_utf8() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"fallback"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .header("content-type", "text/plain; charset=made-up-encoding")
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let text = resp.text_with_charset("utf-8").await.unwrap();
        assert_eq!(text, "fallback");
    }

    #[tokio::test]
    async fn into_bytes_stream_yields_data() {
        let body = http_body_util::Full::new(bytes::Bytes::from_static(b"streamed"))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBoxSendBody::from_boxed(body))
            .unwrap();
        let resp = Response::new(inner, "http://example.com/".parse().unwrap());
        let mut stream = resp.into_bytes_stream();

        let chunk = stream.next().await;
        assert!(chunk.is_some());
        assert_eq!(&chunk.unwrap().unwrap()[..], b"streamed");

        let end = stream.next().await;
        assert!(end.is_none());
    }
}
