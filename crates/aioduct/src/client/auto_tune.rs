use http::{Request, Version};
use http_body::Body;

use crate::auto_tune::AutoTuneConfig;
use crate::pool::ProtocolHint;

/// Select the protocol for one finalized request without inspecting or
/// buffering its body.
pub(super) fn select_protocol<B: Body>(
    config: Option<&AutoTuneConfig>,
    explicit_version: Option<Version>,
    explicit_hint: ProtocolHint,
    request: &Request<B>,
) -> ProtocolHint {
    let Some(config) = config else {
        return explicit_hint;
    };
    if explicit_hint != ProtocolHint::Auto
        || explicit_version.is_some()
        // Middleware can set the finalized request version after the builder
        // arguments have been captured. Preserve every non-default version as
        // an explicit transport choice too.
        || request.version() != Version::HTTP_11
    {
        return explicit_hint;
    }

    if request.method() == http::Method::CONNECT
        || request.headers().contains_key(http::header::UPGRADE)
    {
        return explicit_hint;
    }

    request
        .body()
        .size_hint()
        .exact()
        .filter(|size| *size >= config.large_body_threshold)
        .map_or(ProtocolHint::Auto, |_| ProtocolHint::Http1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body::{Frame, SizeHint};
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    #[derive(Clone)]
    struct KnownBody(u64);

    impl Body for KnownBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(None)
        }

        fn size_hint(&self) -> SizeHint {
            let mut hint = SizeHint::new();
            hint.set_exact(self.0);
            hint
        }
    }

    struct UnknownBody;

    impl Body for UnknownBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(None)
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::new()
        }
    }

    fn request<B>(body: B) -> Request<B> {
        Request::new(body)
    }

    #[test]
    fn disabled_policy_preserves_protocol_hint() {
        let request = request(KnownBody(u64::MAX));
        for hint in [ProtocolHint::Auto, ProtocolHint::Http1, ProtocolHint::Http2] {
            assert_eq!(select_protocol(None, None, hint, &request), hint);
        }
    }

    #[test]
    fn large_exact_body_uses_http1() {
        let config = AutoTuneConfig::default();
        let body = KnownBody(16 * 1024 * 1024);
        assert_eq!(
            select_protocol(Some(&config), None, ProtocolHint::Auto, &request(body)),
            ProtocolHint::Http1
        );
    }

    #[test]
    fn threshold_is_inclusive() {
        let config = AutoTuneConfig::default();
        let body = KnownBody(16 * 1024 * 1024 - 1);
        assert_eq!(
            select_protocol(Some(&config), None, ProtocolHint::Auto, &request(body)),
            ProtocolHint::Auto
        );
    }

    #[test]
    fn custom_threshold_is_used() {
        let config = AutoTuneConfig::default().large_body_threshold(64);
        assert_eq!(
            select_protocol(
                Some(&config),
                None,
                ProtocolHint::Auto,
                &request(KnownBody(63))
            ),
            ProtocolHint::Auto
        );
        assert_eq!(
            select_protocol(
                Some(&config),
                None,
                ProtocolHint::Auto,
                &request(KnownBody(64))
            ),
            ProtocolHint::Http1
        );
    }

    #[test]
    fn unknown_body_stays_automatic() {
        let config = AutoTuneConfig::default();
        assert_eq!(
            select_protocol(
                Some(&config),
                None,
                ProtocolHint::Auto,
                &request(UnknownBody)
            ),
            ProtocolHint::Auto
        );
    }

    #[test]
    fn explicit_settings_win() {
        let config = AutoTuneConfig::default();
        let body = KnownBody(16 * 1024 * 1024);
        let request = request(body);
        assert_eq!(
            select_protocol(Some(&config), None, ProtocolHint::Http2, &request),
            ProtocolHint::Http2
        );
        assert_eq!(
            select_protocol(
                Some(&config),
                Some(Version::HTTP_11),
                ProtocolHint::Auto,
                &request
            ),
            ProtocolHint::Auto
        );

        let request = Request::builder()
            .version(Version::HTTP_2)
            .body(KnownBody(16 * 1024 * 1024))
            .expect("request builder should accept HTTP/2 version");
        assert_eq!(
            select_protocol(Some(&config), None, ProtocolHint::Auto, &request),
            ProtocolHint::Auto
        );
    }

    #[test]
    fn upgrade_and_connect_are_automatic() {
        let config = AutoTuneConfig::default();
        let body = KnownBody(16 * 1024 * 1024);
        let upgrade = Request::builder()
            .header(http::header::UPGRADE, "websocket")
            .body(body.clone())
            .expect("request builder should accept test headers");
        assert_eq!(
            select_protocol(Some(&config), None, ProtocolHint::Auto, &upgrade),
            ProtocolHint::Auto
        );

        let connect = Request::builder()
            .method(http::Method::CONNECT)
            .body(body)
            .expect("request builder should accept CONNECT");
        assert_eq!(
            select_protocol(Some(&config), None, ProtocolHint::Auto, &connect),
            ProtocolHint::Auto
        );
    }
}
