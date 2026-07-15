use http::header::HeaderMap;
use http::{Method, Uri, Version};
use http_body_util::BodyExt;

use crate::body::{RequestBodyLocal, RequestBodySend};
use crate::pool::ProtocolHint;

#[derive(Clone)]
struct ReplayableRequestMetadata {
    protocol_hint: Option<ProtocolHint>,
    extended_connect_protocol: Option<hyper::ext::Protocol>,
    deferred_te_trailers: bool,
    deferred_forward_framing: Option<crate::forward::dispatch_plan::DeferredForwardFraming>,
    deferred_forward_trailers: Option<crate::forward::dispatch_plan::DeferredForwardTrailers>,
    deferred_forward_target: Option<crate::forward::dispatch_plan::DeferredForwardTarget>,
    forward_signing_target: Option<crate::forward::dispatch_plan::ForwardSigningTarget>,
}

impl ReplayableRequestMetadata {
    fn capture(extensions: &http::Extensions) -> Self {
        Self {
            protocol_hint: extensions.get::<ProtocolHint>().copied(),
            extended_connect_protocol: extensions.get::<hyper::ext::Protocol>().cloned(),
            deferred_te_trailers: extensions
                .get::<crate::forward::dispatch_plan::DeferredTeTrailers>()
                .is_some(),
            deferred_forward_framing: extensions
                .get::<crate::forward::dispatch_plan::DeferredForwardFraming>()
                .copied(),
            deferred_forward_trailers: extensions
                .get::<crate::forward::dispatch_plan::DeferredForwardTrailers>()
                .cloned(),
            deferred_forward_target: extensions
                .get::<crate::forward::dispatch_plan::DeferredForwardTarget>()
                .cloned(),
            forward_signing_target: extensions
                .get::<crate::forward::dispatch_plan::ForwardSigningTarget>()
                .cloned(),
        }
    }

    fn restore(self, extensions: &mut http::Extensions) {
        if let Some(protocol_hint) = self.protocol_hint {
            extensions.insert(protocol_hint);
        }
        if let Some(protocol) = self.extended_connect_protocol {
            extensions.insert(protocol);
        }
        if self.deferred_te_trailers {
            extensions.insert(crate::forward::dispatch_plan::DeferredTeTrailers);
        }
        if let Some(framing) = self.deferred_forward_framing {
            extensions.insert(framing);
        }
        if let Some(trailers) = self.deferred_forward_trailers {
            extensions.insert(trailers);
        }
        if let Some(target) = self.deferred_forward_target {
            extensions.insert(target);
        }
        if let Some(target) = self.forward_signing_target {
            extensions.insert(target);
        }
    }
}

/// Cloneable request state owned by aioduct and required for replay.
///
/// Unknown user extensions are intentionally excluded: `http::Extensions`
/// cannot be cloned in general, and replay must not guess their semantics.
#[derive(Clone)]
pub(super) struct ReplayableRequestHead {
    method: Method,
    uri: Uri,
    version: Version,
    headers: HeaderMap,
    metadata: ReplayableRequestMetadata,
}

impl ReplayableRequestHead {
    pub(super) fn capture<B>(request: &http::Request<B>) -> Self {
        Self {
            method: request.method().clone(),
            uri: request.uri().clone(),
            version: request.version(),
            headers: request.headers().clone(),
            metadata: ReplayableRequestMetadata::capture(request.extensions()),
        }
    }

    pub(super) fn method(&self) -> &Method {
        &self.method
    }

    pub(super) fn uri(&self) -> &Uri {
        &self.uri
    }

    pub(super) fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub(super) fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    pub(super) fn into_request<B>(self, body: B) -> http::Request<B> {
        let mut request = http::Request::new(body);
        *request.method_mut() = self.method;
        *request.uri_mut() = self.uri;
        *request.version_mut() = self.version;
        *request.headers_mut() = self.headers;
        request
            .headers_mut()
            .remove(http::header::TRANSFER_ENCODING);
        request.headers_mut().remove(http::header::CONTENT_LENGTH);
        self.metadata.restore(request.extensions_mut());
        request
    }
}

pub(super) fn replay_request_send(
    head: ReplayableRequestHead,
    replay_body: &Option<bytes::Bytes>,
) -> http::Request<RequestBodySend> {
    let body = http_body_util::Full::new(replay_body.clone().unwrap_or_default())
        .map_err(|never| match never {})
        .boxed_unsync();
    head.into_request(body)
}

pub(super) fn replay_request_local(
    head: ReplayableRequestHead,
    replay_body: &Option<bytes::Bytes>,
) -> http::Request<RequestBodyLocal> {
    let body = Box::pin(
        http_body_util::Full::new(replay_body.clone().unwrap_or_default())
            .map_err(|never| match never {}),
    );
    head.into_request(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct UnknownExtension(&'static str);

    #[test]
    fn replay_preserves_owned_protocol_metadata_only() {
        let mut request = http::Request::builder()
            .method(Method::CONNECT)
            .uri("https://example.com/tunnel")
            .version(Version::HTTP_2)
            .header(http::header::CONTENT_LENGTH, "7")
            .header(http::header::TRANSFER_ENCODING, "chunked")
            .header("x-request", "preserved")
            .body(())
            .unwrap();
        request.extensions_mut().insert(ProtocolHint::H2c);
        request
            .extensions_mut()
            .insert(hyper::ext::Protocol::from_static("websocket"));
        request
            .extensions_mut()
            .insert(UnknownExtension("not replayable"));

        let replay = ReplayableRequestHead::capture(&request).into_request(());

        assert_eq!(replay.method(), Method::CONNECT);
        assert_eq!(replay.uri(), "https://example.com/tunnel");
        assert_eq!(replay.version(), Version::HTTP_2);
        assert_eq!(replay.headers()["x-request"], "preserved");
        assert!(!replay.headers().contains_key(http::header::CONTENT_LENGTH));
        assert!(
            !replay
                .headers()
                .contains_key(http::header::TRANSFER_ENCODING)
        );
        assert_eq!(
            replay.extensions().get::<ProtocolHint>(),
            Some(&ProtocolHint::H2c)
        );
        assert_eq!(
            replay
                .extensions()
                .get::<hyper::ext::Protocol>()
                .map(hyper::ext::Protocol::as_str),
            Some("websocket")
        );
        assert!(replay.extensions().get::<UnknownExtension>().is_none());
    }
}
