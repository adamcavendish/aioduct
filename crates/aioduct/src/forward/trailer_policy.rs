use http::header::{CONNECTION, HeaderMap, HeaderName, HeaderValue, TRAILER};

use crate::error::Error;
use crate::h2_h3_field_policy::{
    FieldDirection, FieldProtocol, H2H3FieldPolicy, is_forbidden_trailer_field,
};

const PRESERVE_TRAILERS: u8 = 0;
const DECLARED_TRAILERS_ONLY: u8 = 1;
const DISALLOW_TRAILERS: u8 = 2;
const DEFER_TRAILER_POLICY: u8 = 3;
const UNSUPPORTED_H3_TRAILERS: u8 = 4;

#[derive(Clone, Debug)]
pub(crate) struct TrailerPolicy {
    connection_names: Vec<HeaderName>,
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
    declared_names: Vec<HeaderName>,
    direction: FieldDirection,
    response_status: Option<http::StatusCode>,
    native_source: Option<FieldProtocol>,
    strict_protocol: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl Default for TrailerPolicy {
    fn default() -> Self {
        Self::capture(&HeaderMap::new())
    }
}

impl TrailerPolicy {
    pub(crate) fn capture(headers: &HeaderMap) -> Self {
        Self {
            connection_names: connection_options(headers),
            mode: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(PRESERVE_TRAILERS)),
            declared_names: Vec::new(),
            direction: FieldDirection::Request,
            response_status: None,
            native_source: None,
            strict_protocol: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0)),
        }
    }

    pub(crate) fn capture_request_for_version(headers: &HeaderMap, version: http::Version) -> Self {
        Self::capture_with_direction(headers, version, FieldDirection::Request, None)
    }

    pub(crate) fn capture_response_for_version(
        headers: &HeaderMap,
        version: http::Version,
        status: http::StatusCode,
    ) -> Self {
        Self::capture_with_direction(headers, version, FieldDirection::Response, Some(status))
    }

    fn capture_with_direction(
        headers: &HeaderMap,
        version: http::Version,
        direction: FieldDirection,
        response_status: Option<http::StatusCode>,
    ) -> Self {
        let mut policy = Self::capture(headers);
        policy.direction = direction;
        policy.response_status = response_status;
        if let Some(protocol) = FieldProtocol::from_version(version) {
            policy.native_source = Some(protocol);
            policy.require_strict_field_values(protocol);
            if protocol == FieldProtocol::Http3 {
                policy.set_mode(UNSUPPORTED_H3_TRAILERS);
            }
        }
        policy
    }

    pub(crate) fn merge(&mut self, other: Self) {
        // Keep provenance from the captured source. An H2/H3 egress policy
        // must not make translated H1 trailers subject to native strictness.
        let other_mode = other.mode();
        if let Some(protocol) = other.strict_protocol() {
            self.require_strict_field_values(protocol);
        }
        for name in other.connection_names {
            if !self.connection_names.contains(&name) {
                self.connection_names.push(name);
            }
        }
        for name in other.declared_names {
            if !self.declared_names.contains(&name) {
                self.declared_names.push(name);
            }
        }
        if other_mode == UNSUPPORTED_H3_TRAILERS {
            self.set_mode(UNSUPPORTED_H3_TRAILERS);
        } else if other_mode == DISALLOW_TRAILERS && self.mode() != UNSUPPORTED_H3_TRAILERS {
            self.disallow_trailers();
        }
    }

    pub(crate) fn disallow_trailers(&mut self) {
        if self.mode() != UNSUPPORTED_H3_TRAILERS {
            self.set_mode(DISALLOW_TRAILERS);
        }
    }

    pub(crate) fn restrict_to_declaration(&mut self, headers: &HeaderMap) {
        self.declared_names = self.declared_names(headers);
        if self.mode() != UNSUPPORTED_H3_TRAILERS {
            self.set_mode(DECLARED_TRAILERS_ONLY);
        }
    }

    pub(crate) fn defer_to_connection(&mut self, headers: &HeaderMap) {
        self.declared_names = self.declared_names(headers);
        if self.mode() != UNSUPPORTED_H3_TRAILERS {
            self.set_mode(DEFER_TRAILER_POLICY);
        }
    }

    pub(crate) fn select_for_version(&self, version: http::Version) {
        let mode = match version {
            http::Version::HTTP_2 => {
                self.require_strict_field_values_for_version(version);
                PRESERVE_TRAILERS
            }
            http::Version::HTTP_3 => {
                self.require_strict_field_values_for_version(version);
                UNSUPPORTED_H3_TRAILERS
            }
            _ => DECLARED_TRAILERS_ONLY,
        };
        self.set_mode(mode);
    }

    pub(crate) fn connection_names(&self) -> &[HeaderName] {
        &self.connection_names
    }

    pub(crate) fn require_strict_field_values_for_version(&self, version: http::Version) {
        if let Some(protocol) = FieldProtocol::from_version(version) {
            self.require_strict_field_values(protocol);
        }
    }

    pub(crate) fn sanitize_declaration(&self, headers: &mut HeaderMap) {
        if self.mode() == DISALLOW_TRAILERS {
            headers.remove(TRAILER);
            return;
        }

        let declared_names = headers
            .get_all(TRAILER)
            .iter()
            .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
            .map(trim_ows)
            .filter_map(|token| HeaderName::from_bytes(token).ok())
            .filter(|name| !self.is_forbidden(name))
            .collect::<Vec<_>>();

        headers.remove(TRAILER);
        if declared_names.is_empty() {
            return;
        }

        let declaration = declared_names
            .iter()
            .map(HeaderName::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if let Ok(value) = HeaderValue::from_str(&declaration) {
            headers.insert(TRAILER, value);
        }
    }

    pub(crate) fn sanitize_frame<D>(
        &self,
        frame: http_body::Frame<D>,
    ) -> Result<Option<http_body::Frame<D>>, Error> {
        match frame.into_trailers() {
            Ok(mut trailers) => {
                if self.native_source == Some(FieldProtocol::Http3)
                    || self.mode() == UNSUPPORTED_H3_TRAILERS
                {
                    return Err(match self.direction {
                        FieldDirection::Request => Error::Unsupported(
                            "HTTP/3 request trailers are not supported by aioduct".to_owned(),
                        ),
                        FieldDirection::Response => Error::Unsupported(
                            "HTTP/3 response trailers are not supported by aioduct".to_owned(),
                        ),
                    });
                }
                if let Some(source) = self.native_source {
                    self.validate_native_h2_h3_trailers(source, &trailers)?;
                } else if let Some(protocol) = self.strict_protocol() {
                    H2H3FieldPolicy::new(protocol, self.direction)
                        .validate_field_values(&trailers, "trailer")?;
                }
                let mode = self.mode();
                if matches!(mode, PRESERVE_TRAILERS | DECLARED_TRAILERS_ONLY) {
                    let frame_connection_names = connection_options(&trailers);
                    let forbidden = trailers
                        .keys()
                        .filter(|name| self.is_forbidden(name))
                        .cloned()
                        .collect::<Vec<_>>();
                    for name in forbidden {
                        trailers.remove(name);
                    }
                    for name in self
                        .connection_names
                        .iter()
                        .chain(frame_connection_names.iter())
                    {
                        trailers.remove(name);
                    }
                    if mode == DECLARED_TRAILERS_ONLY {
                        let undeclared = trailers
                            .keys()
                            .filter(|name| !self.declared_names.contains(name))
                            .cloned()
                            .collect::<Vec<_>>();
                        for name in undeclared {
                            trailers.remove(name);
                        }
                    }
                } else {
                    trailers.clear();
                }
                Ok((!trailers.is_empty()).then(|| http_body::Frame::trailers(trailers)))
            }
            Err(frame) => Ok(Some(frame)),
        }
    }

    fn is_forbidden(&self, name: &HeaderName) -> bool {
        is_forbidden_trailer_field(self.direction, name) || self.connection_names.contains(name)
    }

    fn validate_native_h2_h3_trailers(
        &self,
        source: FieldProtocol,
        trailers: &HeaderMap,
    ) -> Result<(), Error> {
        H2H3FieldPolicy::new(source, self.direction)
            .validate_trailers(self.response_status, trailers)?;

        let frame_connection_names = connection_options(trailers);
        let connection_specific = trailers.keys().find(|name| {
            self.connection_names.contains(name) || frame_connection_names.contains(name)
        });
        if let Some(name) = connection_specific {
            return Err(Error::InvalidHeader(format!(
                "{} {} trailers contain connection-specific field `{name}`",
                source.name(),
                match self.direction {
                    FieldDirection::Request => "request",
                    FieldDirection::Response => "response",
                }
            )));
        }
        Ok(())
    }

    fn declared_names(&self, headers: &HeaderMap) -> Vec<HeaderName> {
        headers
            .get_all(TRAILER)
            .iter()
            .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
            .map(trim_ows)
            .filter_map(|token| HeaderName::from_bytes(token).ok())
            .filter(|name| !self.is_forbidden(name))
            .collect()
    }

    fn mode(&self) -> u8 {
        self.mode.load(std::sync::atomic::Ordering::Acquire)
    }

    fn strict_protocol(&self) -> Option<FieldProtocol> {
        match self
            .strict_protocol
            .load(std::sync::atomic::Ordering::Acquire)
        {
            2 => Some(FieldProtocol::Http2),
            3 => Some(FieldProtocol::Http3),
            _ => None,
        }
    }

    fn require_strict_field_values(&self, protocol: FieldProtocol) {
        self.strict_protocol.store(
            match protocol {
                FieldProtocol::Http2 => 2,
                FieldProtocol::Http3 => 3,
            },
            std::sync::atomic::Ordering::Release,
        );
    }

    fn set_mode(&self, mode: u8) {
        self.mode.store(mode, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) fn connection_options(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(CONNECTION)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .map(trim_ows)
        .filter_map(|token| HeaderName::from_bytes(token).ok())
        .collect()
}

fn trim_ows(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(b' ' | b'\t')) {
        value = &value[1..];
    }
    while matches!(value.last(), Some(b' ' | b'\t')) {
        value = &value[..value.len() - 1];
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h2_h3_field_policy::{FORBIDDEN_REQUEST_TRAILER_FIELDS, FORBIDDEN_TRAILER_FIELDS};

    #[test]
    fn inherited_connection_options_are_removed_from_later_trailers() {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("x-initial-secret"));
        let policy = TrailerPolicy::capture(&headers);

        let mut trailers = HeaderMap::new();
        trailers.insert("x-initial-secret", HeaderValue::from_static("remove"));
        trailers.insert(CONNECTION, HeaderValue::from_static("x-frame-secret"));
        trailers.insert("x-frame-secret", HeaderValue::from_static("remove"));
        trailers.insert("x-checksum", HeaderValue::from_static("preserve"));

        let trailers = policy
            .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();

        assert!(!trailers.contains_key("x-initial-secret"));
        assert!(!trailers.contains_key("x-frame-secret"));
        assert!(!trailers.contains_key(CONNECTION));
        assert_eq!(trailers["x-checksum"], "preserve");
    }

    #[test]
    fn disallowed_trailers_remove_declarations_and_frames() {
        let mut headers = HeaderMap::new();
        headers.insert(TRAILER, HeaderValue::from_static("x-checksum"));
        let mut policy = TrailerPolicy::capture(&headers);
        policy.disallow_trailers();
        policy.sanitize_declaration(&mut headers);

        let mut trailers = HeaderMap::new();
        trailers.insert("x-checksum", HeaderValue::from_static("remove"));
        assert!(!headers.contains_key(TRAILER));
        assert!(
            policy
                .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn strict_trailer_policy_rejects_surrounding_whitespace() {
        for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
            for value in [b" leading".as_slice(), b"trailing\t".as_slice()] {
                let policy = TrailerPolicy::capture_request_for_version(&HeaderMap::new(), version);
                let mut trailers = HeaderMap::new();
                trailers.insert("x-test", HeaderValue::from_bytes(value).unwrap());
                assert!(
                    policy
                        .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                        .is_err(),
                    "accepted trailer field {value:?} for {version:?}"
                );
            }
        }
    }

    #[test]
    fn native_h2_and_h3_trailers_reject_connection_specific_fields() {
        for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
            for name in [
                "connection",
                "keep-alive",
                "proxy-connection",
                "transfer-encoding",
                "upgrade",
                "http2-settings",
            ] {
                let policy = TrailerPolicy::capture_request_for_version(&HeaderMap::new(), version);
                let mut trailers = HeaderMap::new();
                trailers.insert(
                    HeaderName::from_static(name),
                    HeaderValue::from_static("x-secret"),
                );

                let error = policy
                    .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                    .unwrap_err();
                match version {
                    http::Version::HTTP_2 => assert!(
                        matches!(error, Error::InvalidHeader(_)),
                        "accepted native {version:?} trailer field {name}"
                    ),
                    http::Version::HTTP_3 => assert!(
                        matches!(error, Error::Unsupported(_)),
                        "did not fail closed for native {version:?} trailer field {name}"
                    ),
                    _ => unreachable!(),
                }
            }

            let mut source_headers = HeaderMap::new();
            source_headers.insert(CONNECTION, HeaderValue::from_static("x-secret"));
            let policy = TrailerPolicy::capture_request_for_version(&source_headers, version);
            let mut trailers = HeaderMap::new();
            trailers.insert("x-secret", HeaderValue::from_static("must-reject"));
            assert!(
                policy
                    .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                    .is_err(),
                "accepted a native {version:?} trailer field named by Connection"
            );
        }
    }

    #[test]
    fn native_h2_and_h3_trailers_reject_every_forbidden_field() {
        for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
            for direction in [FieldDirection::Request, FieldDirection::Response] {
                for name in FORBIDDEN_TRAILER_FIELDS {
                    let policy = TrailerPolicy::capture_with_direction(
                        &HeaderMap::new(),
                        version,
                        direction,
                        (direction == FieldDirection::Response).then_some(http::StatusCode::OK),
                    );
                    let mut trailers = HeaderMap::new();
                    trailers.insert(
                        HeaderName::from_static(name),
                        HeaderValue::from_static("forbidden"),
                    );

                    let error = policy
                        .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                        .unwrap_err();
                    match version {
                        http::Version::HTTP_2 => assert!(
                            matches!(error, Error::InvalidHeader(_)),
                            "accepted native {version:?} {direction:?} trailer field {name}"
                        ),
                        http::Version::HTTP_3 => assert!(
                            matches!(error, Error::Unsupported(_)),
                            "did not fail closed for native {version:?} {direction:?} trailer field {name}"
                        ),
                        _ => unreachable!(),
                    }
                }
            }

            for name in FORBIDDEN_REQUEST_TRAILER_FIELDS {
                let policy = TrailerPolicy::capture_request_for_version(&HeaderMap::new(), version);
                let mut trailers = HeaderMap::new();
                trailers.insert(
                    HeaderName::from_static(name),
                    HeaderValue::from_static("forbidden"),
                );
                assert!(
                    policy
                        .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                        .is_err(),
                    "accepted native {version:?} request trailer field {name}"
                );
            }
        }
    }

    #[test]
    fn native_h2_validation_and_h1_translation_share_directional_trailer_policy() {
        struct Case {
            name: &'static str,
            request_allowed: bool,
            response_allowed: bool,
        }

        let cases = [
            Case {
                name: "allow",
                request_allowed: false,
                response_allowed: false,
            },
            Case {
                name: "content-language",
                request_allowed: false,
                response_allowed: false,
            },
            Case {
                name: "last-modified",
                request_allowed: false,
                response_allowed: false,
            },
            Case {
                name: "accept-ranges",
                request_allowed: false,
                response_allowed: true,
            },
            Case {
                name: "authentication-info",
                request_allowed: false,
                response_allowed: true,
            },
            Case {
                name: "etag",
                request_allowed: false,
                response_allowed: true,
            },
            Case {
                name: "expires",
                request_allowed: false,
                response_allowed: false,
            },
            Case {
                name: "proxy-authentication-info",
                request_allowed: false,
                response_allowed: true,
            },
            Case {
                name: "x-safe-extension",
                request_allowed: true,
                response_allowed: true,
            },
        ];

        let version = http::Version::HTTP_2;
        for case in &cases {
            for (direction, allowed) in [
                (FieldDirection::Request, case.request_allowed),
                (FieldDirection::Response, case.response_allowed),
            ] {
                let status =
                    (direction == FieldDirection::Response).then_some(http::StatusCode::OK);
                let mut trailers = HeaderMap::new();
                trailers.insert(
                    HeaderName::from_static(case.name),
                    HeaderValue::from_static("value"),
                );

                let native = TrailerPolicy::capture_with_direction(
                    &HeaderMap::new(),
                    version,
                    direction,
                    status,
                )
                .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers.clone()));
                assert_eq!(
                    native.is_ok(),
                    allowed,
                    "unexpected native {version:?} {direction:?} result for {}: {native:?}",
                    case.name
                );

                let mut translated = TrailerPolicy::capture_with_direction(
                    &HeaderMap::new(),
                    http::Version::HTTP_11,
                    direction,
                    status,
                );
                translated.merge(TrailerPolicy::capture_with_direction(
                    &HeaderMap::new(),
                    version,
                    direction,
                    status,
                ));
                let translated = translated
                    .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                    .unwrap();
                assert_eq!(
                    translated.is_some(),
                    allowed,
                    "unexpected H1-to-{version:?} {direction:?} translation for {}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn native_h2_and_h3_204_and_304_response_trailers_are_rejected() {
        for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
            for status in [http::StatusCode::NO_CONTENT, http::StatusCode::NOT_MODIFIED] {
                let policy =
                    TrailerPolicy::capture_response_for_version(&HeaderMap::new(), version, status);
                let mut trailers = HeaderMap::new();
                trailers.insert("x-checksum", HeaderValue::from_static("must-reject"));

                let error = policy
                    .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                    .unwrap_err();
                if version == http::Version::HTTP_3 {
                    assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
                } else {
                    assert!(
                        error.to_string().contains("must not contain trailers"),
                        "unexpected native {version:?} {status} trailer error: {error}"
                    );
                }
            }
        }
    }

    #[test]
    fn h1_trailers_remain_sanitized_when_egress_policy_is_h2() {
        let mut source_headers = HeaderMap::new();
        source_headers.insert(CONNECTION, HeaderValue::from_static("x-initial-secret"));
        let mut policy =
            TrailerPolicy::capture_request_for_version(&source_headers, http::Version::HTTP_11);
        policy.merge(TrailerPolicy::capture_request_for_version(
            &HeaderMap::new(),
            http::Version::HTTP_2,
        ));

        let mut trailers = HeaderMap::new();
        trailers.insert(CONNECTION, HeaderValue::from_static("x-frame-secret"));
        trailers.insert("x-initial-secret", HeaderValue::from_static("remove"));
        trailers.insert("x-frame-secret", HeaderValue::from_static("remove"));
        trailers.insert("keep-alive", HeaderValue::from_static("remove"));
        trailers.insert("proxy-connection", HeaderValue::from_static("remove"));
        trailers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        trailers.insert("upgrade", HeaderValue::from_static("websocket"));
        trailers.insert("http2-settings", HeaderValue::from_static("remove"));
        trailers.insert(http::header::TE, HeaderValue::from_static("gzip"));
        trailers.insert("x-checksum", HeaderValue::from_static("preserve"));

        let trailers = policy
            .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();

        assert_eq!(trailers.len(), 1);
        assert_eq!(trailers["x-checksum"], "preserve");
    }

    #[test]
    fn h3_source_and_sink_trailers_fail_closed_before_sanitization() {
        for (mut policy, context) in [
            (
                TrailerPolicy::capture_request_for_version(
                    &HeaderMap::new(),
                    http::Version::HTTP_3,
                ),
                "source",
            ),
            (
                {
                    let mut policy = TrailerPolicy::capture_request_for_version(
                        &HeaderMap::new(),
                        http::Version::HTTP_11,
                    );
                    policy.merge(TrailerPolicy::capture_request_for_version(
                        &HeaderMap::new(),
                        http::Version::HTTP_3,
                    ));
                    policy
                },
                "sink",
            ),
        ] {
            policy.disallow_trailers();
            let mut trailers = HeaderMap::new();
            trailers.insert(
                http::header::AUTHORIZATION,
                HeaderValue::from_static("must-not-be-silently-removed"),
            );

            let error = policy
                .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                .unwrap_err();
            assert!(
                matches!(error, Error::Unsupported(_)),
                "{context}: {error:?}"
            );
            assert!(error.to_string().contains("HTTP/3 request trailers"));
        }
    }
}
