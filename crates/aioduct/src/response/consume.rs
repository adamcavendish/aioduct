use super::Response;

impl Response {
    /// Consume the response and return its fixed response-body carrier.
    ///
    /// The carrier keeps aioduct's private request continuation attached while
    /// allowing callers to hold and poll the body without exposing an observer
    /// type parameter.
    pub fn into_body(self) -> super::ResponseBodySend {
        self.inner.into_body()
    }

    /// Convert the response into an async byte stream.
    pub fn into_bytes_stream(self) -> crate::body::BodyStreamSend {
        crate::body::BodyStreamSend::from_response_body(self.inner.into_body())
    }

    /// Convert the response into a Server-Sent Events stream.
    pub fn into_sse_stream(self) -> crate::sse::SseStreamSend {
        crate::sse::SseStreamSend::from_response_body(self.inner.into_body())
    }

    /// Perform an HTTP upgrade (e.g., WebSocket) on this response.
    pub async fn upgrade(mut self) -> Result<crate::upgrade::UpgradedSend, crate::error::Error> {
        let permit = self.take_upgrade_stream_permit();
        crate::upgrade::on_upgrade(&mut self.inner, permit).await
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use crate::response::{Response, ResponseBodySend};
    use http_body_util::BodyExt;

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
