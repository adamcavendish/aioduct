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
    let engine = HttpEngineSend::<TokioRuntime, TcpConnector>::new(TcpConnector);
    let builder = generic_build(&engine);
    assert!(builder.is_ok());
}
