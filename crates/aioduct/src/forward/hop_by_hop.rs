use base64::Engine as _;
use http::header::{
    CONNECTION, CONTENT_LENGTH, HeaderMap, HeaderValue, TE, TRANSFER_ENCODING, UPGRADE,
};

use crate::error::Error;
use crate::h2_h3_field_policy::{FieldDirection, H2H3FieldPolicy};

use super::trailer_policy::{TrailerPolicy, connection_options};

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "transfer-encoding",
    "upgrade",
    "http2-settings",
];
const HTTP2_SETTINGS: &str = "http2-settings";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HeaderProtocol {
    Negotiated,
    Http10,
    Http11,
    Http2,
    Http3,
}

#[derive(Clone)]
pub(crate) struct ResponseHeaderPolicy {
    protocol: HeaderProtocol,
    request_method: http::Method,
    downstream_accepts_trailers: bool,
    h1_upgrade_offer: Option<H1UpgradeOffer>,
}

#[derive(Clone)]
pub(crate) struct ResponseBodyPolicy {
    trailers: TrailerPolicy,
    allow_body: bool,
    eager_validate_before_handoff: bool,
    body_length: crate::message_framing::BodyLengthValidator,
}

impl ResponseBodyPolicy {
    pub(crate) fn sanitize_frame<D>(
        &mut self,
        frame: http_body::Frame<D>,
    ) -> Result<Option<http_body::Frame<D>>, Error>
    where
        D: bytes::Buf,
    {
        if !self.allow_body {
            return match frame.into_data() {
                Ok(_) => Err(Error::InvalidHeader(
                    "upstream sent a payload frame for a response that cannot contain a body"
                        .to_owned(),
                )),
                Err(frame) => match frame.into_trailers() {
                    Ok(trailers) => {
                        self.body_length.finish("forwarded response")?;
                        self.trailers
                            .sanitize_frame(http_body::Frame::trailers(trailers))
                    }
                    Err(_) => Err(Error::InvalidHeader(
                        "upstream sent an unsupported frame for a response that cannot contain a body"
                            .to_owned(),
                    )),
                },
            };
        }
        if let Some(data) = frame.data_ref() {
            self.body_length
                .record(data.remaining(), "forwarded response")?;
        } else if frame.trailers_ref().is_some() {
            self.body_length.finish("forwarded response")?;
        }
        self.trailers.sanitize_frame(frame)
    }

    pub(crate) fn finish(&mut self) -> Result<(), Error> {
        self.body_length.finish("forwarded response")
    }

    pub(crate) fn is_end_stream(&self, inner_is_end_stream: bool) -> bool {
        self.body_length.is_end_stream(inner_is_end_stream)
    }

    #[cfg(test)]
    pub(crate) fn allows_body(&self) -> bool {
        self.allow_body
    }

    pub(crate) fn size_hint(&self, inner: http_body::SizeHint) -> http_body::SizeHint {
        if self.allow_body {
            self.body_length.size_hint(inner)
        } else {
            http_body::SizeHint::with_exact(0)
        }
    }

    pub(crate) fn eager_validate_before_handoff(&self) -> bool {
        self.eager_validate_before_handoff
    }
}

#[derive(Clone, Debug)]
pub(crate) struct H1UpgradeOffer {
    protocols: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct H1UpgradeSelection {
    protocols: Vec<String>,
}

impl H1UpgradeOffer {
    pub(crate) fn intersect(&self, allowed: &Self) -> Self {
        Self {
            protocols: self
                .protocols
                .iter()
                .filter(|protocol| allowed.protocols.contains(protocol))
                .cloned()
                .collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.protocols.is_empty()
    }

    pub(crate) fn constrain_request_headers(&self, headers: &mut HeaderMap) -> Result<(), Error> {
        let protocols = self.protocols.join(", ");
        let value = HeaderValue::from_bytes(protocols.as_bytes()).map_err(|_| {
            Error::InvalidHeader(
                "shared downstream and upstream Upgrade protocols are invalid".to_owned(),
            )
        })?;
        headers.insert(UPGRADE, value);
        Ok(())
    }
}

impl ResponseHeaderPolicy {
    pub(crate) fn new(
        protocol: HeaderProtocol,
        request_method: http::Method,
        downstream_accepts_trailers: bool,
        h1_upgrade_offer: Option<H1UpgradeOffer>,
    ) -> Self {
        Self {
            protocol,
            request_method,
            downstream_accepts_trailers,
            h1_upgrade_offer,
        }
    }

    pub(crate) fn downstream_version(&self) -> http::Version {
        match self.protocol {
            HeaderProtocol::Http10 => http::Version::HTTP_10,
            HeaderProtocol::Http11 | HeaderProtocol::Negotiated => http::Version::HTTP_11,
            HeaderProtocol::Http2 => http::Version::HTTP_2,
            HeaderProtocol::Http3 => http::Version::HTTP_3,
        }
    }

    pub(crate) fn sanitize(
        &self,
        status: http::StatusCode,
        headers: &mut HeaderMap,
        source_version: Option<http::Version>,
    ) -> Result<ResponseBodyPolicy, Error> {
        let source_is_h2_or_h3 = source_version
            .is_none_or(|version| matches!(version, http::Version::HTTP_2 | http::Version::HTTP_3));
        if matches!(self.protocol, HeaderProtocol::Http2 | HeaderProtocol::Http3)
            && source_is_h2_or_h3
        {
            validate_h2_h3_header_block(
                source_version.unwrap_or_else(|| self.downstream_version()),
                headers,
                false,
            )?;
        }
        let transfer_coded = headers.contains_key(TRANSFER_ENCODING);
        let content_length = crate::message_framing::validate_response_content_length(
            &self.request_method,
            status,
            headers,
            "forwarded response",
        )?;
        let mut trailer_policy = TrailerPolicy::capture_response_for_version(
            headers,
            source_version.unwrap_or_else(|| self.downstream_version()),
            status,
        );
        if self.protocol == HeaderProtocol::Http3 {
            trailer_policy.select_for_version(http::Version::HTTP_3);
        } else if self.protocol == HeaderProtocol::Http2 {
            trailer_policy.require_strict_field_values_for_version(self.downstream_version());
        }
        let response_allows_body = response_allows_body(&self.request_method, status);
        if self.protocol == HeaderProtocol::Http10
            || (self.protocol == HeaderProtocol::Http11
                && (!response_allows_body || !self.downstream_accepts_trailers))
        {
            trailer_policy.disallow_trailers();
        }
        if !response_allows_body {
            sanitize_bodyless_framing(status, &self.request_method, headers);
        }
        let preserve_upgrade = if status == http::StatusCode::SWITCHING_PROTOCOLS {
            self.validate_switching_protocols(headers)?;
            true
        } else {
            false
        };
        let upgrade_values = preserve_upgrade.then(|| collect_upgrade_values(headers));

        strip_hop_by_hop_with_policy(headers, &trailer_policy);

        if matches!(self.protocol, HeaderProtocol::Http2 | HeaderProtocol::Http3)
            && !source_is_h2_or_h3
        {
            validate_h2_h3_header_block(self.downstream_version(), headers, false)?;
        }

        if let Some(Some(values)) = upgrade_values {
            restore_h1_upgrade(headers, values);
        }
        if self.protocol == HeaderProtocol::Http11
            && response_allows_body
            && self.downstream_accepts_trailers
        {
            if headers.contains_key(http::header::TRAILER) {
                headers.remove(CONTENT_LENGTH);
                headers.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
            }
            trailer_policy.restrict_to_declaration(headers);
        }
        Ok(ResponseBodyPolicy {
            trailers: trailer_policy,
            allow_body: response_allows_body,
            eager_validate_before_handoff: self.request_method == http::Method::HEAD
                || matches!(
                    status,
                    http::StatusCode::NO_CONTENT
                        | http::StatusCode::RESET_CONTENT
                        | http::StatusCode::NOT_MODIFIED
                ),
            body_length: if response_allows_body {
                // RFC 9112 section 6.3 gives Transfer-Encoding precedence
                // over Content-Length. The received length is still parsed
                // above so malformed or conflicting values fail before
                // handoff, but a valid field removed during hop-by-hop
                // sanitization must not constrain the decoded body stream.
                crate::message_framing::BodyLengthValidator::from_expected(if transfer_coded {
                    None
                } else {
                    content_length
                })
            } else {
                crate::message_framing::BodyLengthValidator::exact(0)
            },
        })
    }

    pub(crate) fn upgrade_selection(
        &self,
        status: http::StatusCode,
        headers: &HeaderMap,
    ) -> Result<Option<H1UpgradeSelection>, Error> {
        if status != http::StatusCode::SWITCHING_PROTOCOLS {
            return Ok(None);
        }
        self.validate_switching_protocols(headers)?;
        let values = collect_upgrade_values(headers).ok_or_else(|| {
            Error::InvalidHeader(
                "101 Switching Protocols requires a valid non-empty Upgrade field".to_owned(),
            )
        })?;
        Ok(Some(H1UpgradeSelection {
            protocols: normalized_upgrade_protocols(&values),
        }))
    }

    pub(crate) fn validate_preserved_upgrade_selection(
        &self,
        status: http::StatusCode,
        headers: &HeaderMap,
        expected: Option<&H1UpgradeSelection>,
    ) -> Result<(), Error> {
        let actual = self.upgrade_selection(status, headers)?;
        if actual.as_ref() != expected {
            return Err(Error::InvalidHeader(
                "response hook changed the upstream-selected upgrade protocol".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_response_hook_status(
        &self,
        upstream_status: http::StatusCode,
        forwarded_status: http::StatusCode,
    ) -> Result<(), Error> {
        if forwarded_status.is_informational()
            && (upstream_status != http::StatusCode::SWITCHING_PROTOCOLS
                || forwarded_status != http::StatusCode::SWITCHING_PROTOCOLS)
        {
            return Err(Error::InvalidHeader(
                "response hook cannot create or change a terminal informational response"
                    .to_owned(),
            ));
        }
        if self.request_method == http::Method::CONNECT
            && upstream_status.is_success() != forwarded_status.is_success()
        {
            return Err(Error::InvalidHeader(
                "response hook cannot change whether a CONNECT response establishes a tunnel"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_switching_protocols(&self, headers: &HeaderMap) -> Result<(), Error> {
        if self.protocol != HeaderProtocol::Http11 {
            return Err(Error::InvalidHeader(
                "101 Switching Protocols is valid only on an HTTP/1.1 downstream connection"
                    .to_owned(),
            ));
        }
        let Some(offer) = self.h1_upgrade_offer.as_ref() else {
            return Err(Error::InvalidHeader(
                "upstream sent an unsolicited 101 Switching Protocols response".to_owned(),
            ));
        };
        if !connection_options(headers).contains(&UPGRADE) {
            return Err(Error::InvalidHeader(
                "101 Switching Protocols requires `Connection: upgrade`".to_owned(),
            ));
        }
        let values = collect_upgrade_values(headers).ok_or_else(|| {
            Error::InvalidHeader(
                "101 Switching Protocols requires a valid non-empty Upgrade field".to_owned(),
            )
        })?;
        let selected = normalized_upgrade_protocols(&values);
        for selected in selected {
            if !offer.protocols.iter().any(|offered| offered == &selected) {
                return Err(Error::InvalidHeader(format!(
                    "upstream selected unoffered upgrade protocol `{selected}`"
                )));
            }
        }
        Ok(())
    }
}

fn response_allows_body(method: &http::Method, status: http::StatusCode) -> bool {
    *method != http::Method::HEAD
        && !status.is_informational()
        && status != http::StatusCode::NO_CONTENT
        && status != http::StatusCode::RESET_CONTENT
        && status != http::StatusCode::NOT_MODIFIED
        && !(*method == http::Method::CONNECT && status.is_success())
}

fn sanitize_bodyless_framing(
    status: http::StatusCode,
    method: &http::Method,
    headers: &mut HeaderMap,
) {
    headers.remove(TRANSFER_ENCODING);
    if *method == http::Method::HEAD || status == http::StatusCode::NOT_MODIFIED {
        return;
    }
    headers.remove(CONTENT_LENGTH);
    if status == http::StatusCode::RESET_CONTENT {
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    }
}

pub(crate) fn downstream_accepts_response_trailers(
    version: http::Version,
    headers: &HeaderMap,
) -> bool {
    match version {
        http::Version::HTTP_10 => false,
        http::Version::HTTP_11 => {
            connection_options(headers).contains(&TE)
                && headers.get_all(TE).iter().any(|value| {
                    value.to_str().ok().is_some_and(|value| {
                        value
                            .split(',')
                            .map(str::trim)
                            .any(|token| token.eq_ignore_ascii_case("trailers"))
                    })
                })
        }
        http::Version::HTTP_2 | http::Version::HTTP_3 => true,
        _ => false,
    }
}

pub(crate) fn is_h1_upgrade(headers: &HeaderMap) -> bool {
    h1_upgrade_offer(headers).is_some()
}

pub(crate) fn h1_upgrade_offer(headers: &HeaderMap) -> Option<H1UpgradeOffer> {
    if !connection_options(headers).contains(&UPGRADE) {
        return None;
    }
    let values = collect_upgrade_values(headers)?;
    Some(H1UpgradeOffer {
        protocols: normalized_upgrade_protocols(&values),
    })
}

pub(crate) fn is_h2c_upgrade(headers: &HeaderMap) -> bool {
    collect_upgrade_values(headers).is_some_and(|values| contains_h2c(&values))
}

pub(crate) fn sanitize_request(
    headers: &mut HeaderMap,
    protocol: HeaderProtocol,
    h1_upgrade_request: bool,
    downstream_accepts_trailers: bool,
) -> Result<TrailerPolicy, Error> {
    if matches!(protocol, HeaderProtocol::Http2 | HeaderProtocol::Http3) {
        validate_h2_h3_header_block(header_protocol_version(protocol), headers, true)?;
    }
    crate::message_framing::normalize_content_length(headers, "forwarded request")?;
    if protocol == HeaderProtocol::Http11 {
        validate_h1_transfer_encoding(http::Version::HTTP_11, headers, "request")?;
    }
    let trailer_policy = match protocol {
        HeaderProtocol::Http2 => {
            TrailerPolicy::capture_request_for_version(headers, http::Version::HTTP_2)
        }
        HeaderProtocol::Http3 => {
            TrailerPolicy::capture_request_for_version(headers, http::Version::HTTP_3)
        }
        _ => TrailerPolicy::capture(headers),
    };
    let te_values: Vec<HeaderValue> = headers.get_all(TE).iter().cloned().collect();
    let preserve_h1_te_trailers = protocol == HeaderProtocol::Http11
        && downstream_accepts_trailers
        && !te_values.is_empty()
        && validate_te_trailers(&te_values).is_ok();
    let upgrade_values = if h1_upgrade_request {
        if protocol != HeaderProtocol::Http11 {
            return Err(Error::Unsupported(
                "HTTP/1.1 upgrade fields require HTTP/1.1 egress".to_owned(),
            ));
        }
        if !connection_options(headers).contains(&UPGRADE) {
            return Err(Error::InvalidHeader(
                "HTTP/1.1 upgrade requires `Connection: upgrade`".to_owned(),
            ));
        }
        Some(collect_upgrade_values(headers).ok_or_else(|| {
            Error::InvalidHeader(
                "HTTP/1.1 upgrade requires a valid non-empty Upgrade field".to_owned(),
            )
        })?)
    } else {
        None
    };
    let h2c_settings = upgrade_values
        .as_ref()
        .filter(|values| contains_h2c(values))
        .map(|_| collect_h2c_settings(headers))
        .transpose()?;

    strip_hop_by_hop_with_policy(headers, &trailer_policy);

    if matches!(protocol, HeaderProtocol::Http2 | HeaderProtocol::Http3) && !te_values.is_empty() {
        validate_te_trailers(&te_values)?;
        headers.insert(TE, HeaderValue::from_static("trailers"));
    }

    if let Some(values) = upgrade_values {
        restore_h1_upgrade_request(headers, values, h2c_settings, preserve_h1_te_trailers);
    } else if preserve_h1_te_trailers {
        restore_h1_te_trailers(headers);
    }

    Ok(trailer_policy)
}

pub(crate) fn validate_inbound_request_headers(
    version: http::Version,
    headers: &mut HeaderMap,
) -> Result<(), Error> {
    if matches!(version, http::Version::HTTP_2 | http::Version::HTTP_3) {
        validate_h2_h3_header_block(version, headers, true)?;
    } else if matches!(version, http::Version::HTTP_10 | http::Version::HTTP_11) {
        validate_h1_transfer_encoding(version, headers, "request")?;
    }
    crate::message_framing::normalize_content_length(headers, "inbound request")?;
    Ok(())
}

pub(crate) fn validate_final_request_headers(
    version: http::Version,
    headers: &mut HeaderMap,
) -> Result<Option<u64>, Error> {
    if matches!(version, http::Version::HTTP_2 | http::Version::HTTP_3) {
        validate_h2_h3_header_block(version, headers, true)?;
    } else if matches!(version, http::Version::HTTP_10 | http::Version::HTTP_11) {
        validate_h1_transfer_encoding(version, headers, "request")?;
    }
    crate::message_framing::normalize_content_length(headers, "forwarded request")
}

pub(crate) fn validate_inbound_response_headers(
    version: http::Version,
    request_method: &http::Method,
    status: http::StatusCode,
    headers: &mut HeaderMap,
) -> Result<(), Error> {
    if matches!(version, http::Version::HTTP_2 | http::Version::HTTP_3) {
        validate_h2_h3_header_block(version, headers, false)?;
    } else if matches!(version, http::Version::HTTP_10 | http::Version::HTTP_11)
        && response_allows_body(request_method, status)
    {
        validate_h1_transfer_encoding(version, headers, "response")?;
    }
    crate::message_framing::validate_response_content_length(
        request_method,
        status,
        headers,
        "inbound response",
    )?;
    Ok(())
}

fn validate_h2_h3_header_block(
    version: http::Version,
    headers: &HeaderMap,
    request: bool,
) -> Result<(), Error> {
    let direction = if request {
        FieldDirection::Request
    } else {
        FieldDirection::Response
    };
    let Some(policy) = H2H3FieldPolicy::for_version(version, direction) else {
        return Err(Error::Unsupported(format!(
            "field policy is unavailable for {version:?}"
        )));
    };
    policy.validate_headers(headers)
}

fn header_protocol_version(protocol: HeaderProtocol) -> http::Version {
    match protocol {
        HeaderProtocol::Http2 => http::Version::HTTP_2,
        HeaderProtocol::Http3 => http::Version::HTTP_3,
        _ => unreachable!("header protocol is not HTTP/2 or HTTP/3"),
    }
}

fn validate_h1_transfer_encoding(
    version: http::Version,
    headers: &HeaderMap,
    section: &str,
) -> Result<(), Error> {
    let values = headers
        .get_all(TRANSFER_ENCODING)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .map(trim_ows)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Ok(());
    }
    if version != http::Version::HTTP_11
        || values.len() != 1
        || !values[0].eq_ignore_ascii_case(b"chunked")
    {
        return Err(Error::InvalidHeader(format!(
            "HTTP/1 {section} Transfer-Encoding must contain exactly one `chunked` coding"
        )));
    }
    Ok(())
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

pub(crate) fn strip_hop_by_hop_with_policy(
    headers: &mut HeaderMap,
    trailer_policy: &TrailerPolicy,
) {
    trailer_policy.sanitize_declaration(headers);

    if headers.contains_key(TRANSFER_ENCODING) {
        headers.remove(CONTENT_LENGTH);
    }

    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    for name in trailer_policy.connection_names() {
        headers.remove(name);
    }
}

fn collect_upgrade_values(headers: &HeaderMap) -> Option<Vec<HeaderValue>> {
    let values: Vec<HeaderValue> = headers.get_all(UPGRADE).iter().cloned().collect();
    if values.is_empty()
        || values
            .iter()
            .any(|value| !valid_upgrade_value(value.as_bytes()))
    {
        return None;
    }
    Some(values)
}

fn valid_upgrade_value(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    value.split(',').all(|protocol| {
        let protocol = protocol.trim();
        if protocol.is_empty() {
            return false;
        }
        let mut components = protocol.split('/');
        let name = components.next().unwrap_or_default();
        let version = components.next();
        components.next().is_none()
            && valid_token(name.as_bytes())
            && version.is_none_or(|version| valid_token(version.as_bytes()))
    })
}

fn contains_h2c(values: &[HeaderValue]) -> bool {
    values.iter().any(|value| {
        value.to_str().ok().is_some_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|protocol| protocol.eq_ignore_ascii_case("h2c"))
        })
    })
}

fn normalized_upgrade_protocols(values: &[HeaderValue]) -> Vec<String> {
    values
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn collect_h2c_settings(headers: &HeaderMap) -> Result<HeaderValue, Error> {
    if !connection_options(headers)
        .iter()
        .any(|name| name == HTTP2_SETTINGS)
    {
        return Err(Error::InvalidHeader(
            "h2c upgrade requires `Connection: http2-settings`".to_owned(),
        ));
    }

    let values: Vec<HeaderValue> = headers.get_all(HTTP2_SETTINGS).iter().cloned().collect();
    let [value] = values.as_slice() else {
        return Err(Error::InvalidHeader(
            "h2c upgrade requires exactly one HTTP2-Settings field".to_owned(),
        ));
    };
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value.as_bytes())
        .map_err(|_| {
            Error::InvalidHeader(
                "HTTP2-Settings must be unpadded base64url-encoded SETTINGS data".to_owned(),
            )
        })?;
    if decoded.len() % 6 != 0 {
        return Err(Error::InvalidHeader(
            "HTTP2-Settings payload must contain complete six-byte settings parameters".to_owned(),
        ));
    }
    Ok(value.clone())
}

pub(crate) fn valid_token(value: &[u8]) -> bool {
    !value.is_empty()
        && value.iter().all(|byte| {
            matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

fn validate_te_trailers(values: &[HeaderValue]) -> Result<(), Error> {
    let valid = values.iter().all(|value| {
        value.to_str().ok().is_some_and(|value| {
            let mut tokens = value.split(',').map(str::trim).peekable();
            tokens.peek().is_some()
                && tokens.all(|token| !token.is_empty() && token.eq_ignore_ascii_case("trailers"))
        })
    });
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidHeader(
            "HTTP/2 and HTTP/3 TE fields may contain only `trailers`".to_owned(),
        ))
    }
}

pub(crate) fn deferred_te(
    headers: &HeaderMap,
    downstream_accepts_trailers: bool,
) -> Option<super::dispatch_plan::DeferredTe> {
    let values: Vec<HeaderValue> = headers.get_all(TE).iter().cloned().collect();
    if values.is_empty() {
        return None;
    }
    Some(if validate_te_trailers(&values).is_ok() {
        if downstream_accepts_trailers {
            super::dispatch_plan::DeferredTe::Trailers
        } else {
            super::dispatch_plan::DeferredTe::TrailersForH2OrH3
        }
    } else {
        super::dispatch_plan::DeferredTe::InvalidForH2OrH3
    })
}

fn restore_h1_upgrade(headers: &mut HeaderMap, values: Vec<HeaderValue>) {
    headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    for value in values {
        headers.append(UPGRADE, value);
    }
}

fn restore_h1_upgrade_request(
    headers: &mut HeaderMap,
    values: Vec<HeaderValue>,
    h2c_settings: Option<HeaderValue>,
    preserve_te_trailers: bool,
) {
    restore_h1_upgrade(headers, values);
    if let Some(settings) = h2c_settings {
        headers.insert(
            CONNECTION,
            if preserve_te_trailers {
                HeaderValue::from_static("upgrade, http2-settings, TE")
            } else {
                HeaderValue::from_static("upgrade, http2-settings")
            },
        );
        headers.insert(HTTP2_SETTINGS, settings);
    } else if preserve_te_trailers {
        headers.insert(CONNECTION, HeaderValue::from_static("upgrade, TE"));
    }
    if preserve_te_trailers {
        headers.insert(TE, HeaderValue::from_static("trailers"));
    }
}

pub(crate) fn restore_h1_te_trailers(headers: &mut HeaderMap) {
    headers.insert(CONNECTION, HeaderValue::from_static("TE"));
    headers.insert(TE, HeaderValue::from_static("trailers"));
}

#[cfg(test)]
mod tests;
