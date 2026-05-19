use super::*;
use crate::client::HttpEngineSend;
use crate::error::Error;
use crate::runtime::tokio_rt::{TcpConnector, TokioRuntime};

fn assert_http_client<C: HttpClient>() {}

#[test]
fn http_engine_implements_http_client() {
    assert_http_client::<HttpEngineSend<TokioRuntime, TcpConnector>>();
}

fn generic_build<C: HttpClient>(client: &C) -> Result<C::RequestBuilder, Error> {
    client
        .get("http://example.com")?
        .header(
            http::header::ACCEPT,
            http::header::HeaderValue::from_static("text/html"),
        )
        .body("test")
        .timeout(std::time::Duration::from_secs(5));
    client.post("http://example.com")
}

#[test]
fn generic_request_building() {
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::new();
    let builder = generic_build(&engine);
    assert!(builder.is_ok());
}

#[cfg(feature = "compio")]
mod compio_local_tests {
    use super::*;
    use crate::client::HttpEngineLocal;
    use crate::runtime::compio_rt::{CompioRuntime, TcpConnector as CompioTcpConnector};
    use http_body_util::BodyExt;

    #[test]
    fn http_engine_local_implements_http_client() {
        assert_http_client::<HttpEngineLocal<CompioRuntime, CompioTcpConnector>>();
    }

    #[test]
    fn generic_request_building_local() {
        let engine = HttpEngineLocal::<CompioRuntime, CompioTcpConnector>::new();
        let builder = generic_build(&engine);
        assert!(builder.is_ok());
    }

    #[test]
    fn local_client_all_methods() {
        let engine = HttpEngineLocal::<CompioRuntime, CompioTcpConnector>::new();
        assert!(engine.get("http://example.com").is_ok());
        assert!(engine.head("http://example.com").is_ok());
        assert!(engine.post("http://example.com").is_ok());
        assert!(engine.put("http://example.com").is_ok());
        assert!(engine.patch("http://example.com").is_ok());
        assert!(engine.delete("http://example.com").is_ok());
    }

    #[test]
    fn local_client_invalid_url() {
        let engine = HttpEngineLocal::<CompioRuntime, CompioTcpConnector>::new();
        assert!(engine.get("not a valid url\n").is_err());
    }

    /// Helper to construct a `Response<ResponseBodyLocal>` for trait tests.
    fn make_local_response_with_headers(
        status: http::StatusCode,
        headers: http::header::HeaderMap,
        body_bytes: &[u8],
    ) -> crate::response::Response<crate::body::ResponseBodyLocal> {
        use crate::response::ResponseBodySend;

        let body = http_body_util::Full::new(bytes::Bytes::from(body_bytes.to_vec()))
            .map_err(|never| match never {})
            .boxed_unsync();
        let send_body = ResponseBodySend::from_boxed(body);
        let mut inner = http::Response::builder()
            .status(status)
            .body(send_body)
            .unwrap();
        *inner.headers_mut() = headers;
        let resp = crate::response::Response::new(inner, "http://example.com/".parse().unwrap());
        resp.into_local()
    }

    #[test]
    fn response_ext_status() {
        use super::ResponseExt;

        let resp = make_local_response_with_headers(
            http::StatusCode::NOT_FOUND,
            http::header::HeaderMap::new(),
            b"",
        );
        assert_eq!(ResponseExt::status(&resp), http::StatusCode::NOT_FOUND);

        let resp = make_local_response_with_headers(
            http::StatusCode::OK,
            http::header::HeaderMap::new(),
            b"hello",
        );
        assert_eq!(ResponseExt::status(&resp), http::StatusCode::OK);
    }

    #[test]
    fn response_ext_headers() {
        use super::ResponseExt;

        let mut headers = http::header::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            http::header::HeaderName::from_static("x-custom"),
            http::header::HeaderValue::from_static("custom-value"),
        );

        let resp = make_local_response_with_headers(http::StatusCode::OK, headers, b"body");
        let resp_headers = ResponseExt::headers(&resp);
        assert_eq!(
            resp_headers.get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            resp_headers
                .get(http::header::HeaderName::from_static("x-custom"))
                .unwrap(),
            "custom-value"
        );
    }

    #[test]
    fn response_ext_bytes() {
        use super::ResponseExt;

        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resp = make_local_response_with_headers(
                http::StatusCode::OK,
                http::header::HeaderMap::new(),
                b"response bytes content",
            );
            let bytes = ResponseExt::bytes(resp).await.unwrap();
            assert_eq!(bytes, "response bytes content");
        });
    }

    #[test]
    fn response_ext_text() {
        use super::ResponseExt;

        compio_runtime::Runtime::new().unwrap().block_on(async {
            let resp = make_local_response_with_headers(
                http::StatusCode::OK,
                http::header::HeaderMap::new(),
                b"response text content",
            );
            let text = ResponseExt::text(resp).await.unwrap();
            assert_eq!(text, "response text content");
        });
    }
}
