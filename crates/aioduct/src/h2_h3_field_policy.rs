use http::header::TE;
use http::{HeaderMap, HeaderName, StatusCode, Version};

use crate::error::Error;

const CONNECTION_SPECIFIC_FIELDS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "http2-settings",
];

// RFC 9110 section 6.5 excludes fields needed for framing, routing,
// authentication, request processing, response control, or content handling.
pub(crate) const FORBIDDEN_TRAILER_FIELDS: &[&str] = &[
    "accept",
    "accept-charset",
    "accept-encoding",
    "accept-language",
    "age",
    "allow",
    "authorization",
    "cache-control",
    "connection",
    "content-disposition",
    "content-encoding",
    "content-language",
    "content-length",
    "content-location",
    "content-range",
    "content-type",
    "cookie",
    "date",
    "early-data",
    "expect",
    "expires",
    "from",
    "host",
    "http2-settings",
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-range",
    "if-unmodified-since",
    "keep-alive",
    "last-modified",
    "location",
    "max-forwards",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "range",
    "referer",
    "retry-after",
    "set-cookie",
    "strict-transport-security",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "vary",
    "warning",
    "www-authenticate",
];

// These response-only fields have definitions that permit response-trailer
// use, but they have no request-trailer meaning.
pub(crate) const FORBIDDEN_REQUEST_TRAILER_FIELDS: &[&str] = &[
    "accept-ranges",
    "authentication-info",
    "etag",
    "proxy-authentication-info",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FieldProtocol {
    Http2,
    Http3,
}

impl FieldProtocol {
    pub(crate) fn from_version(version: Version) -> Option<Self> {
        match version {
            Version::HTTP_2 => Some(Self::Http2),
            Version::HTTP_3 => Some(Self::Http3),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Http2 => "HTTP/2",
            Self::Http3 => "HTTP/3",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FieldDirection {
    Request,
    Response,
}

impl FieldDirection {
    fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct H2H3FieldPolicy {
    protocol: FieldProtocol,
    direction: FieldDirection,
}

impl H2H3FieldPolicy {
    pub(crate) fn new(protocol: FieldProtocol, direction: FieldDirection) -> Self {
        Self {
            protocol,
            direction,
        }
    }

    pub(crate) fn for_version(version: Version, direction: FieldDirection) -> Option<Self> {
        FieldProtocol::from_version(version).map(|protocol| Self::new(protocol, direction))
    }

    pub(crate) fn validate_headers(self, headers: &HeaderMap) -> Result<(), Error> {
        self.validate_field_values(headers, "header")?;
        if let Some(name) = connection_specific_field(headers) {
            return Err(Error::InvalidHeader(format!(
                "{} {} header contains connection-specific field `{name}`",
                self.protocol.name(),
                self.direction.name(),
            )));
        }

        let te_values = headers.get_all(TE).iter().collect::<Vec<_>>();
        if te_values.is_empty() {
            return Ok(());
        }
        if self.direction == FieldDirection::Request && has_only_te_trailers(&te_values) {
            return Ok(());
        }

        let message = if self.direction == FieldDirection::Request {
            format!(
                "{} request TE fields may contain only `trailers`",
                self.protocol.name()
            )
        } else {
            format!("{} responses must not contain `TE`", self.protocol.name())
        };
        Err(Error::InvalidHeader(message))
    }

    pub(crate) fn validate_trailers(
        self,
        status: Option<StatusCode>,
        trailers: &HeaderMap,
    ) -> Result<(), Error> {
        self.validate_field_values(trailers, "trailer")?;
        if self.direction == FieldDirection::Response
            && let Some(status) = status
            && matches!(status, StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED)
        {
            return Err(Error::InvalidHeader(format!(
                "{} {} responses must not contain trailers",
                self.protocol.name(),
                status
            )));
        }
        if let Some(name) = connection_specific_field(trailers) {
            return Err(Error::InvalidHeader(format!(
                "{} {} trailers contain connection-specific field `{name}`",
                self.protocol.name(),
                self.direction.name(),
            )));
        }
        if let Some(name) = trailers
            .keys()
            .find(|name| is_forbidden_trailer_field(self.direction, name))
        {
            return Err(Error::InvalidHeader(format!(
                "{} {} trailers contain forbidden field `{name}`",
                self.protocol.name(),
                self.direction.name(),
            )));
        }
        Ok(())
    }

    pub(crate) fn validate_field_values(
        self,
        headers: &HeaderMap,
        section: &str,
    ) -> Result<(), Error> {
        for (name, value) in headers {
            if matches!(value.as_bytes().first(), Some(b' ' | b'\t'))
                || matches!(value.as_bytes().last(), Some(b' ' | b'\t'))
            {
                return Err(Error::InvalidHeader(format!(
                    "{} {} {section} field `{name}` has leading or trailing whitespace",
                    self.protocol.name(),
                    self.direction.name(),
                )));
            }
        }
        Ok(())
    }
}

pub(crate) fn is_forbidden_trailer_field(direction: FieldDirection, name: &HeaderName) -> bool {
    FORBIDDEN_TRAILER_FIELDS
        .iter()
        .any(|forbidden| name.as_str().eq_ignore_ascii_case(forbidden))
        || (direction == FieldDirection::Request
            && FORBIDDEN_REQUEST_TRAILER_FIELDS
                .iter()
                .any(|forbidden| name.as_str().eq_ignore_ascii_case(forbidden)))
}

fn connection_specific_field(headers: &HeaderMap) -> Option<&HeaderName> {
    headers.keys().find(|name| {
        CONNECTION_SPECIFIC_FIELDS
            .iter()
            .any(|field| name.as_str().eq_ignore_ascii_case(field))
    })
}

fn has_only_te_trailers(values: &[&http::HeaderValue]) -> bool {
    values.iter().all(|value| {
        let mut tokens = value.as_bytes().split(|byte| *byte == b',').map(trim_ows);
        let Some(first) = tokens.next() else {
            return false;
        };
        !first.is_empty()
            && first.eq_ignore_ascii_case(b"trailers")
            && tokens.all(|token| !token.is_empty() && token.eq_ignore_ascii_case(b"trailers"))
    })
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
    use http::HeaderValue;

    struct TrailerCase {
        name: &'static str,
        request_allowed: bool,
        response_allowed: bool,
    }

    const TRAILER_CASES: &[TrailerCase] = &[
        TrailerCase {
            name: "allow",
            request_allowed: false,
            response_allowed: false,
        },
        TrailerCase {
            name: "content-language",
            request_allowed: false,
            response_allowed: false,
        },
        TrailerCase {
            name: "last-modified",
            request_allowed: false,
            response_allowed: false,
        },
        TrailerCase {
            name: "accept-ranges",
            request_allowed: false,
            response_allowed: true,
        },
        TrailerCase {
            name: "authentication-info",
            request_allowed: false,
            response_allowed: true,
        },
        TrailerCase {
            name: "etag",
            request_allowed: false,
            response_allowed: true,
        },
        TrailerCase {
            name: "expires",
            request_allowed: false,
            response_allowed: false,
        },
        TrailerCase {
            name: "proxy-authentication-info",
            request_allowed: false,
            response_allowed: true,
        },
        TrailerCase {
            name: "x-safe-extension",
            request_allowed: true,
            response_allowed: true,
        },
    ];

    #[test]
    fn trailer_policy_is_protocol_consistent_and_direction_aware() {
        for protocol in [FieldProtocol::Http2, FieldProtocol::Http3] {
            for case in TRAILER_CASES {
                let mut trailers = HeaderMap::new();
                trailers.insert(
                    HeaderName::from_static(case.name),
                    HeaderValue::from_static("value"),
                );
                for (direction, allowed) in [
                    (FieldDirection::Request, case.request_allowed),
                    (FieldDirection::Response, case.response_allowed),
                ] {
                    let result = H2H3FieldPolicy::new(protocol, direction).validate_trailers(
                        (direction == FieldDirection::Response).then_some(StatusCode::OK),
                        &trailers,
                    );
                    assert_eq!(
                        result.is_ok(),
                        allowed,
                        "unexpected {protocol:?} {direction:?} result for {}: {result:?}",
                        case.name
                    );
                }
            }
        }
    }

    #[test]
    fn trailer_only_restrictions_do_not_reject_regular_headers() {
        for protocol in [FieldProtocol::Http2, FieldProtocol::Http3] {
            for direction in [FieldDirection::Request, FieldDirection::Response] {
                for case in TRAILER_CASES {
                    let mut headers = HeaderMap::new();
                    headers.insert(
                        HeaderName::from_static(case.name),
                        HeaderValue::from_static("value"),
                    );
                    H2H3FieldPolicy::new(protocol, direction)
                        .validate_headers(&headers)
                        .unwrap_or_else(|error| {
                            panic!(
                                "rejected {protocol:?} {direction:?} header {}: {error}",
                                case.name
                            )
                        });
                }
            }
        }
    }

    #[test]
    fn http2_settings_is_connection_specific_for_h2_and_h3() {
        let mut headers = HeaderMap::new();
        headers.insert("http2-settings", HeaderValue::from_static("settings"));

        for protocol in [FieldProtocol::Http2, FieldProtocol::Http3] {
            for direction in [FieldDirection::Request, FieldDirection::Response] {
                let error = H2H3FieldPolicy::new(protocol, direction)
                    .validate_headers(&headers)
                    .unwrap_err();
                assert!(error.to_string().contains("connection-specific"), "{error}");
            }
        }
    }
}
