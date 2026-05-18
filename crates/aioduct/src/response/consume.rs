use crate::body::RequestBodySend;

use super::Response;

impl Response {
    /// Consume the response and return the raw hyper body.
    pub fn into_body(self) -> RequestBodySend {
        self.inner.into_body().into_boxed()
    }

    /// Convert the response into an async byte stream.
    pub fn into_bytes_stream(self) -> crate::body::BodyStreamSend {
        crate::body::BodyStreamSend::with_observer(
            self.inner.into_body().into_boxed(),
            self.observer_ctx,
        )
    }

    /// Convert the response into a Server-Sent Events stream.
    pub fn into_sse_stream(self) -> crate::sse::SseStreamSend {
        crate::sse::SseStreamSend::new(self.inner.into_body().into_boxed())
    }

    /// Perform an HTTP upgrade (e.g., WebSocket) on this response.
    pub async fn upgrade(mut self) -> Result<crate::upgrade::Upgraded, crate::error::Error> {
        crate::upgrade::on_upgrade(&mut self.inner).await
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use crate::error::Error;
    use crate::observer::{ConnectionEvent, RequestEvent, RequestObserver, RequestPhase};
    use crate::response::{BodyObserverCtx, Response, ResponseBodySend};
    use http_body_util::BodyExt;
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
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
        let boxed: crate::body::RequestBodySend = http_body_util::BodyExt::boxed_unsync(ErrorBody);
        let inner = http::Response::builder()
            .status(200)
            .body(ResponseBodySend::from_boxed(boxed))
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
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
        let body = http_body_util::Full::new(bytes::Bytes::from(vec![0x63, 0x61, 0x66, 0xe9]))
            .map_err(|never| match never {})
            .boxed_unsync();
        let inner = http::Response::builder()
            .status(200)
            .header("content-type", "text/plain; charset=iso-8859-1")
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
            .body(ResponseBodySend::from_boxed(body))
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
