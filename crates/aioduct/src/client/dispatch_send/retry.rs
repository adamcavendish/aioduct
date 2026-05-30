use bytes::Bytes;
use http::header::HeaderMap;
use http::{Method, Uri, Version};
use http_body_util::BodyExt;

use crate::body::RequestBodySend;

pub(super) fn retry_request_from_parts(
    method: Method,
    uri: Uri,
    version: Version,
    headers: HeaderMap,
    replay_body: &Option<Bytes>,
) -> http::Request<RequestBodySend> {
    let retry_body_bytes = replay_body.as_ref().cloned().unwrap_or_else(Bytes::new);
    let body: RequestBodySend = http_body_util::Full::new(retry_body_bytes)
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut retry_req = http::Request::new(body);
    *retry_req.method_mut() = method;
    *retry_req.uri_mut() = uri;
    *retry_req.headers_mut() = headers;
    *retry_req.version_mut() = version;
    retry_req
}
