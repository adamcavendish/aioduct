use super::*;
use http::header::TRAILER;

use crate::h2_h3_field_policy::{FORBIDDEN_REQUEST_TRAILER_FIELDS, FORBIDDEN_TRAILER_FIELDS};

fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let trailer_policy = TrailerPolicy::capture(headers);
    strip_hop_by_hop_with_policy(headers, &trailer_policy);
}

fn sanitize_trailer_frame<D>(frame: http_body::Frame<D>) -> http_body::Frame<D> {
    TrailerPolicy::default()
        .sanitize_frame(frame)
        .unwrap_or_else(|error| {
            panic!("the default trailer policy accepts valid field values: {error}")
        })
        .unwrap_or_else(|| panic!("the default policy preserves non-empty safe trailers"))
}

#[test]
fn strips_every_connection_value_and_named_field() {
    let mut headers = HeaderMap::new();
    headers.append(CONNECTION, HeaderValue::from_static("X-First, keep-alive"));
    headers.append(CONNECTION, HeaderValue::from_static("x-SECOND, bad token"));
    headers.insert("x-first", HeaderValue::from_static("secret-1"));
    headers.insert("x-second", HeaderValue::from_static("secret-2"));
    headers.insert("bad-token", HeaderValue::from_static("unrelated"));
    headers.insert("content-type", HeaderValue::from_static("text/plain"));

    strip_hop_by_hop(&mut headers);

    assert!(!headers.contains_key(CONNECTION));
    assert!(!headers.contains_key("x-first"));
    assert!(!headers.contains_key("x-second"));
    assert!(headers.contains_key("bad-token"));
    assert!(headers.contains_key("content-type"));
}

#[test]
fn strips_all_known_connection_fields_including_orphan_upgrade() {
    let mut headers = HeaderMap::new();
    for name in HOP_BY_HOP {
        headers.insert(*name, HeaderValue::from_static("value"));
    }

    strip_hop_by_hop(&mut headers);

    assert!(headers.is_empty());
}

#[test]
fn stripping_transfer_encoding_also_removes_content_length() {
    let mut headers = HeaderMap::new();
    headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
    headers.insert("content-length", HeaderValue::from_static("7"));

    strip_hop_by_hop(&mut headers);

    assert!(!headers.contains_key("transfer-encoding"));
    assert!(!headers.contains_key("content-length"));
}

#[test]
fn strips_hop_by_hop_fields_from_trailer_frames() {
    let mut trailers = HeaderMap::new();
    trailers.insert(CONNECTION, HeaderValue::from_static("x-secret"));
    trailers.insert("x-secret", HeaderValue::from_static("remove"));
    trailers.insert("x-checksum", HeaderValue::from_static("preserve"));

    let trailers = sanitize_trailer_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
        .into_trailers()
        .unwrap();

    assert!(!trailers.contains_key(CONNECTION));
    assert!(!trailers.contains_key("x-secret"));
    assert_eq!(trailers["x-checksum"], "preserve");
}

#[test]
fn strips_every_forbidden_field_from_trailer_frames() {
    let mut trailers = HeaderMap::new();
    for name in FORBIDDEN_TRAILER_FIELDS
        .iter()
        .chain(FORBIDDEN_REQUEST_TRAILER_FIELDS)
    {
        trailers.insert(*name, HeaderValue::from_static("forbidden"));
    }
    trailers.insert("x-checksum", HeaderValue::from_static("preserve"));

    let trailers = sanitize_trailer_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
        .into_trailers()
        .unwrap();

    for name in FORBIDDEN_TRAILER_FIELDS
        .iter()
        .chain(FORBIDDEN_REQUEST_TRAILER_FIELDS)
    {
        assert!(!trailers.contains_key(*name), "preserved forbidden {name}");
    }
    assert_eq!(trailers["x-checksum"], "preserve");
}

#[test]
fn trailer_declaration_preserves_only_allowed_fields() {
    for protocol in [
        HeaderProtocol::Http11,
        HeaderProtocol::Http2,
        HeaderProtocol::Http3,
    ] {
        let mut headers = HeaderMap::new();
        if protocol == HeaderProtocol::Http11 {
            headers.insert(CONNECTION, HeaderValue::from_static("x-hop-secret"));
        }
        headers.append(
            TRAILER,
            HeaderValue::from_static(
                "x-checksum, Content-Length, Host, Authorization, x-hop-secret",
            ),
        );
        headers.append(
            TRAILER,
            HeaderValue::from_static("Content-Type, x-post-process"),
        );

        sanitize_request(&mut headers, protocol, false, false).unwrap();

        let expected = if protocol == HeaderProtocol::Http11 {
            "x-checksum, x-post-process"
        } else {
            "x-checksum, x-hop-secret, x-post-process"
        };
        assert_eq!(headers[TRAILER], expected);
    }
}

#[test]
fn response_trailer_declaration_preserves_only_allowed_fields() {
    let mut headers = HeaderMap::new();
    headers.insert(
        TRAILER,
        HeaderValue::from_static(
            "x-checksum, Set-Cookie, Strict-Transport-Security, WWW-Authenticate",
        ),
    );

    ResponseHeaderPolicy::new(HeaderProtocol::Http2, http::Method::GET, true, None)
        .sanitize(http::StatusCode::OK, &mut headers, None)
        .unwrap();

    assert_eq!(headers[TRAILER], "x-checksum");
}

#[test]
fn strips_strict_transport_security_from_response_trailers() {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "strict-transport-security",
        HeaderValue::from_static("max-age=0"),
    );
    trailers.insert("x-checksum", HeaderValue::from_static("preserve"));

    let trailers = sanitize_trailer_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
        .into_trailers()
        .unwrap();

    assert!(!trailers.contains_key("strict-transport-security"));
    assert_eq!(trailers["x-checksum"], "preserve");
}

#[test]
fn preserves_only_canonical_te_trailers_for_h2_and_h3() {
    for protocol in [HeaderProtocol::Http2, HeaderProtocol::Http3] {
        let mut headers = HeaderMap::new();
        headers.append(TE, HeaderValue::from_static("Trailers"));
        headers.append(TE, HeaderValue::from_static("trailers, TRAILERS"));

        sanitize_request(&mut headers, protocol, false, false).unwrap();

        assert_eq!(headers.get_all(TE).iter().count(), 1);
        assert_eq!(headers.get(TE).unwrap(), "trailers");
    }
}

#[test]
fn strips_te_for_http10_and_deferred_egress() {
    for protocol in [HeaderProtocol::Http10, HeaderProtocol::Negotiated] {
        let mut headers = HeaderMap::new();
        headers.insert(TE, HeaderValue::from_static("trailers"));
        sanitize_request(&mut headers, protocol, false, false).unwrap();
        assert!(!headers.contains_key(TE));
        assert!(!headers.contains_key(CONNECTION));
    }
}

#[test]
fn regenerates_canonical_h11_trailer_negotiation() {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONNECTION,
        HeaderValue::from_static("keep-alive, TE, x-secret"),
    );
    headers.append(TE, HeaderValue::from_static("Trailers"));
    headers.append(TE, HeaderValue::from_static("trailers, TRAILERS"));
    headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    headers.insert("x-secret", HeaderValue::from_static("remove"));

    sanitize_request(&mut headers, HeaderProtocol::Http11, false, true).unwrap();

    assert_eq!(headers.get(CONNECTION).unwrap(), "TE");
    assert_eq!(headers.get_all(TE).iter().count(), 1);
    assert_eq!(headers.get(TE).unwrap(), "trailers");
    assert!(!headers.contains_key("keep-alive"));
    assert!(!headers.contains_key("x-secret"));
}

#[test]
fn strips_h11_te_when_downstream_did_not_negotiate_trailers() {
    let mut headers = HeaderMap::new();
    headers.insert(TE, HeaderValue::from_static("trailers"));

    sanitize_request(&mut headers, HeaderProtocol::Http11, false, false).unwrap();

    assert!(!headers.contains_key(TE));
    assert!(!headers.contains_key(CONNECTION));
}

#[test]
fn strips_non_trailer_te_for_h11() {
    let mut headers = HeaderMap::new();
    headers.insert(CONNECTION, HeaderValue::from_static("TE"));
    headers.insert(TE, HeaderValue::from_static("gzip"));

    sanitize_request(&mut headers, HeaderProtocol::Http11, false, true).unwrap();

    assert!(!headers.contains_key(TE));
    assert!(!headers.contains_key(CONNECTION));
}

#[test]
fn rejects_non_trailer_te_for_h2_and_h3() {
    for protocol in [HeaderProtocol::Http2, HeaderProtocol::Http3] {
        for value in ["gzip", "trailers, deflate", "", "trailers,"] {
            let mut headers = HeaderMap::new();
            headers.insert(TE, HeaderValue::from_str(value).unwrap());
            assert!(sanitize_request(&mut headers, protocol, false, false).is_err());
        }
    }
}

#[test]
fn valid_upgrade_preserves_only_required_fields() {
    let mut headers = HeaderMap::new();
    headers.append(CONNECTION, HeaderValue::from_static("keep-alive, Upgrade"));
    headers.append(CONNECTION, HeaderValue::from_static("x-secret"));
    headers.append(UPGRADE, HeaderValue::from_static("websocket"));
    headers.append(UPGRADE, HeaderValue::from_static("example/2"));
    headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    headers.insert("x-secret", HeaderValue::from_static("remove"));

    sanitize_request(&mut headers, HeaderProtocol::Http11, true, false).unwrap();

    assert_eq!(headers.get(CONNECTION).unwrap(), "upgrade");
    assert_eq!(headers.get_all(UPGRADE).iter().count(), 2);
    assert!(!headers.contains_key("keep-alive"));
    assert!(!headers.contains_key("x-secret"));
}

#[test]
fn upgrade_and_trailer_negotiation_restore_only_required_fields() {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONNECTION,
        HeaderValue::from_static("keep-alive, Upgrade, TE, x-secret"),
    );
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(TE, HeaderValue::from_static("trailers"));
    headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    headers.insert("x-secret", HeaderValue::from_static("remove"));

    sanitize_request(&mut headers, HeaderProtocol::Http11, true, true).unwrap();

    assert_eq!(headers.get(CONNECTION).unwrap(), "upgrade, TE");
    assert_eq!(headers.get(UPGRADE).unwrap(), "websocket");
    assert_eq!(headers.get(TE).unwrap(), "trailers");
    assert!(!headers.contains_key("keep-alive"));
    assert!(!headers.contains_key("x-secret"));
}

#[test]
fn h2c_upgrade_preserves_valid_http2_settings() {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONNECTION,
        HeaderValue::from_static("keep-alive, Upgrade, HTTP2-Settings"),
    );
    headers.insert(UPGRADE, HeaderValue::from_static("h2c"));
    headers.insert("http2-settings", HeaderValue::from_static("AAEAAAAA"));

    sanitize_request(&mut headers, HeaderProtocol::Http11, true, false).unwrap();

    assert_eq!(headers.get(CONNECTION).unwrap(), "upgrade, http2-settings");
    assert_eq!(headers.get(UPGRADE).unwrap(), "h2c");
    assert_eq!(headers.get("http2-settings").unwrap(), "AAEAAAAA");
}

#[test]
fn h2c_upgrade_rejects_missing_duplicate_or_invalid_http2_settings() {
    for settings in [vec![], vec!["AAEAAAAA", "AAEAAAAA"], vec!["not+url/base64"]] {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONNECTION,
            HeaderValue::from_static("upgrade, http2-settings"),
        );
        headers.insert(UPGRADE, HeaderValue::from_static("h2c"));
        for value in settings {
            headers.append("http2-settings", HeaderValue::from_str(value).unwrap());
        }

        assert!(sanitize_request(&mut headers, HeaderProtocol::Http11, true, false).is_err());
    }

    let mut unnamed = HeaderMap::new();
    unnamed.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    unnamed.insert(UPGRADE, HeaderValue::from_static("h2c"));
    unnamed.insert("http2-settings", HeaderValue::from_static("AAEAAAAA"));
    assert!(sanitize_request(&mut unnamed, HeaderProtocol::Http11, true, false).is_err());
}

#[test]
fn orphan_and_malformed_upgrades_are_not_detected() {
    for upgrade in ["web socket", "websocket/", "websocket/1/2", ""] {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_str(upgrade).unwrap());
        assert!(!is_h1_upgrade(&headers));
        sanitize_request(&mut headers, HeaderProtocol::Http11, false, false).unwrap();
        assert!(!headers.contains_key(CONNECTION));
        assert!(!headers.contains_key(UPGRADE));
    }
}

#[test]
fn response_preserves_only_valid_successful_h1_upgrade() {
    let mut request_headers = HeaderMap::new();
    request_headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    request_headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    let policy = ResponseHeaderPolicy::new(
        HeaderProtocol::Http11,
        http::Method::GET,
        false,
        h1_upgrade_offer(&request_headers),
    );

    let mut success = HeaderMap::new();
    success.insert(CONNECTION, HeaderValue::from_static("upgrade, x-hop"));
    success.insert(UPGRADE, HeaderValue::from_static("websocket"));
    success.insert("x-hop", HeaderValue::from_static("remove"));
    policy
        .sanitize(http::StatusCode::SWITCHING_PROTOCOLS, &mut success, None)
        .unwrap();
    assert_eq!(success.get(CONNECTION).unwrap(), "upgrade");
    assert_eq!(success.get(UPGRADE).unwrap(), "websocket");
    assert!(!success.contains_key("x-hop"));

    for (status, upgrade) in [
        (http::StatusCode::BAD_REQUEST, "websocket"),
        (http::StatusCode::SWITCHING_PROTOCOLS, "web socket"),
    ] {
        let mut rejected = HeaderMap::new();
        rejected.insert(CONNECTION, HeaderValue::from_static("upgrade, x-hop"));
        rejected.insert(UPGRADE, HeaderValue::from_str(upgrade).unwrap());
        rejected.insert("x-hop", HeaderValue::from_static("remove"));
        let result = policy.sanitize(status, &mut rejected, None);
        if status == http::StatusCode::SWITCHING_PROTOCOLS {
            assert!(result.is_err());
        } else {
            result.unwrap();
            assert!(rejected.is_empty());
        }
    }
}

#[test]
fn response_rejects_unsolicited_incomplete_and_mismatched_switches() {
    let mut offered_headers = HeaderMap::new();
    offered_headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    offered_headers.insert(UPGRADE, HeaderValue::from_static("websocket, h2c"));
    let offered = h1_upgrade_offer(&offered_headers);

    let cases = [
        (
            HeaderProtocol::Http11,
            None,
            Some("upgrade"),
            Some("websocket"),
        ),
        (
            HeaderProtocol::Http11,
            offered.clone(),
            None,
            Some("websocket"),
        ),
        (
            HeaderProtocol::Http11,
            offered.clone(),
            Some("upgrade"),
            None,
        ),
        (
            HeaderProtocol::Http11,
            offered.clone(),
            Some("upgrade"),
            Some("other"),
        ),
        (
            HeaderProtocol::Http2,
            offered,
            Some("upgrade"),
            Some("websocket"),
        ),
    ];

    for (protocol, offer, connection, upgrade) in cases {
        let policy = ResponseHeaderPolicy::new(protocol, http::Method::GET, false, offer);
        let mut headers = HeaderMap::new();
        if let Some(connection) = connection {
            headers.insert(CONNECTION, HeaderValue::from_static(connection));
        }
        if let Some(upgrade) = upgrade {
            headers.insert(UPGRADE, HeaderValue::from_static(upgrade));
        }
        assert!(
            policy
                .sanitize(http::StatusCode::SWITCHING_PROTOCOLS, &mut headers, None,)
                .is_err(),
            "accepted invalid 101 for {protocol:?}"
        );
    }
}

#[test]
fn response_preserves_multiple_offered_upgrade_layers_in_order() {
    let mut offered_headers = HeaderMap::new();
    offered_headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    offered_headers.insert(
        UPGRADE,
        HeaderValue::from_static("transport/1, application/2"),
    );
    let policy = ResponseHeaderPolicy::new(
        HeaderProtocol::Http11,
        http::Method::GET,
        false,
        h1_upgrade_offer(&offered_headers),
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    response_headers.insert(
        UPGRADE,
        HeaderValue::from_static("transport/1, application/2"),
    );

    policy
        .sanitize(
            http::StatusCode::SWITCHING_PROTOCOLS,
            &mut response_headers,
            Some(http::Version::HTTP_11),
        )
        .unwrap();

    assert_eq!(
        response_headers.get(UPGRADE).unwrap(),
        "transport/1, application/2"
    );
}

#[test]
fn response_hook_cannot_change_connect_tunnel_establishment() {
    let connect_policy =
        ResponseHeaderPolicy::new(HeaderProtocol::Http11, http::Method::CONNECT, false, None);

    let error = connect_policy
        .validate_response_hook_status(
            http::StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            http::StatusCode::OK,
        )
        .unwrap_err();
    assert!(
        matches!(error, Error::InvalidHeader(message) if message.contains("establishes a tunnel"))
    );

    let error = connect_policy
        .validate_response_hook_status(http::StatusCode::OK, http::StatusCode::BAD_GATEWAY)
        .unwrap_err();
    assert!(
        matches!(error, Error::InvalidHeader(message) if message.contains("establishes a tunnel"))
    );

    connect_policy
        .validate_response_hook_status(
            http::StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            http::StatusCode::BAD_GATEWAY,
        )
        .unwrap();
    connect_policy
        .validate_response_hook_status(http::StatusCode::OK, http::StatusCode::CREATED)
        .unwrap();

    ResponseHeaderPolicy::new(HeaderProtocol::Http11, http::Method::GET, false, None)
        .validate_response_hook_status(http::StatusCode::BAD_GATEWAY, http::StatusCode::OK)
        .unwrap();
}

#[test]
fn h2_and_h3_reject_connection_specific_fields_instead_of_repairing_them() {
    for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
        for name in [
            CONNECTION.as_str(),
            "keep-alive",
            "proxy-connection",
            TRANSFER_ENCODING.as_str(),
            UPGRADE.as_str(),
            HTTP2_SETTINGS,
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                http::header::HeaderName::from_static(name),
                HeaderValue::from_static("invalid"),
            );
            assert!(validate_inbound_request_headers(version, &mut headers.clone()).is_err());
            assert!(
                validate_inbound_response_headers(
                    version,
                    &http::Method::GET,
                    http::StatusCode::OK,
                    &mut headers,
                )
                .is_err()
            );
        }

        let mut invalid_request_te = HeaderMap::new();
        invalid_request_te.insert(TE, HeaderValue::from_static("gzip"));
        assert!(validate_inbound_request_headers(version, &mut invalid_request_te).is_err());

        let mut response_te = HeaderMap::new();
        response_te.insert(TE, HeaderValue::from_static("trailers"));
        assert!(
            validate_inbound_response_headers(
                version,
                &http::Method::GET,
                http::StatusCode::OK,
                &mut response_te,
            )
            .is_err()
        );
    }
}

#[test]
fn h1_hop_by_hop_response_fields_are_removed_before_h2_h3_validation() {
    for protocol in [HeaderProtocol::Http2, HeaderProtocol::Http3] {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("close, x-private"));
        headers.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
        headers.insert("x-private", HeaderValue::from_static("remove"));
        headers.insert("x-end-to-end", HeaderValue::from_static("preserve"));

        ResponseHeaderPolicy::new(protocol, http::Method::GET, true, None)
            .sanitize(
                http::StatusCode::OK,
                &mut headers,
                Some(http::Version::HTTP_11),
            )
            .unwrap();

        assert!(!headers.contains_key(CONNECTION));
        assert!(!headers.contains_key(TRANSFER_ENCODING));
        assert!(!headers.contains_key("x-private"));
        assert_eq!(headers["x-end-to-end"], "preserve");
    }
}

#[test]
fn h2_and_h3_reject_whitespace_around_header_values() {
    for version in [http::Version::HTTP_2, http::Version::HTTP_3] {
        for value in [b" leading".as_slice(), b"trailing\t".as_slice()] {
            let mut request_headers = HeaderMap::new();
            request_headers.insert("x-test", HeaderValue::from_bytes(value).unwrap());
            assert!(
                validate_inbound_request_headers(version, &mut request_headers).is_err(),
                "accepted request field {value:?} for {version:?}"
            );

            let mut response_headers = HeaderMap::new();
            response_headers.insert("x-test", HeaderValue::from_bytes(value).unwrap());
            assert!(
                validate_inbound_response_headers(
                    version,
                    &http::Method::GET,
                    http::StatusCode::OK,
                    &mut response_headers,
                )
                .is_err(),
                "accepted response field {value:?} for {version:?}"
            );
        }
    }
}

#[test]
fn h1_accepts_only_one_chunked_transfer_coding_for_body_messages() {
    for value in ["gzip", "gzip, chunked", "chunked, chunked", "chunked,"] {
        let mut request_headers = HeaderMap::new();
        request_headers.insert(TRANSFER_ENCODING, HeaderValue::from_static(value));
        assert!(
            validate_inbound_request_headers(http::Version::HTTP_11, &mut request_headers,)
                .is_err(),
            "accepted request Transfer-Encoding {value:?}"
        );

        let mut response_headers = HeaderMap::new();
        response_headers.insert(TRANSFER_ENCODING, HeaderValue::from_static(value));
        assert!(
            validate_inbound_response_headers(
                http::Version::HTTP_11,
                &http::Method::GET,
                http::StatusCode::OK,
                &mut response_headers,
            )
            .is_err(),
            "accepted response Transfer-Encoding {value:?}"
        );
    }

    let mut chunked = HeaderMap::new();
    chunked.insert(TRANSFER_ENCODING, HeaderValue::from_static("ChUnKeD"));
    validate_inbound_request_headers(http::Version::HTTP_11, &mut chunked).unwrap();

    let mut bodyless = HeaderMap::new();
    bodyless.insert(TRANSFER_ENCODING, HeaderValue::from_static("gzip, chunked"));
    validate_inbound_response_headers(
        http::Version::HTTP_11,
        &http::Method::GET,
        http::StatusCode::NO_CONTENT,
        &mut bodyless,
    )
    .unwrap();

    let mut http10 = HeaderMap::new();
    http10.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
    assert!(validate_inbound_request_headers(http::Version::HTTP_10, &mut http10).is_err());
}

#[test]
fn h2_connect_tunnel_handoff_accepts_every_success_status() {
    for status in [
        http::StatusCode::OK,
        http::StatusCode::CREATED,
        http::StatusCode::NO_CONTENT,
        http::StatusCode::from_u16(299).unwrap(),
    ] {
        validate_inbound_response_headers(
            http::Version::HTTP_2,
            &http::Method::CONNECT,
            status,
            &mut HeaderMap::new(),
        )
        .unwrap();
    }

    validate_inbound_response_headers(
        http::Version::HTTP_11,
        &http::Method::CONNECT,
        http::StatusCode::CREATED,
        &mut HeaderMap::new(),
    )
    .unwrap();
}

#[test]
fn every_downstream_protocol_rejects_payload_frames_for_bodyless_responses() {
    for protocol in [
        HeaderProtocol::Http10,
        HeaderProtocol::Http11,
        HeaderProtocol::Http2,
        HeaderProtocol::Http3,
    ] {
        for (method, status, content_length, expected_content_length) in [
            (
                http::Method::HEAD,
                http::StatusCode::OK,
                Some("7"),
                Some("7"),
            ),
            (http::Method::GET, http::StatusCode::NO_CONTENT, None, None),
            (
                http::Method::GET,
                http::StatusCode::RESET_CONTENT,
                Some("0"),
                Some("0"),
            ),
            (
                http::Method::GET,
                http::StatusCode::NOT_MODIFIED,
                Some("7"),
                Some("7"),
            ),
            (http::Method::CONNECT, http::StatusCode::OK, None, None),
        ] {
            let policy = ResponseHeaderPolicy::new(protocol, method.clone(), true, None);
            let mut headers = HeaderMap::new();
            if let Some(content_length) = content_length {
                headers.insert(CONTENT_LENGTH, HeaderValue::from_static(content_length));
            }
            let mut body_policy = policy.sanitize(status, &mut headers, None).unwrap();
            assert!(!body_policy.allows_body());
            assert_eq!(
                body_policy.eager_validate_before_handoff(),
                method == http::Method::HEAD
                    || matches!(
                        status,
                        http::StatusCode::NO_CONTENT
                            | http::StatusCode::RESET_CONTENT
                            | http::StatusCode::NOT_MODIFIED
                    ),
                "unexpected eager validation policy for {protocol:?} {method} {status}"
            );
            assert_eq!(
                headers
                    .get(CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok()),
                expected_content_length,
                "unexpected framing for {protocol:?} {status}"
            );
            assert!(
                body_policy
                    .sanitize_frame(http_body::Frame::data(bytes::Bytes::from_static(b"x")))
                    .is_err(),
                "accepted a payload for {protocol:?} {status}"
            );
        }
    }
}

#[test]
fn every_downstream_protocol_rejects_native_h2_204_and_304_trailers() {
    for protocol in [
        HeaderProtocol::Http10,
        HeaderProtocol::Http11,
        HeaderProtocol::Http2,
        HeaderProtocol::Http3,
    ] {
        for status in [http::StatusCode::NO_CONTENT, http::StatusCode::NOT_MODIFIED] {
            let policy = ResponseHeaderPolicy::new(protocol, http::Method::GET, true, None);
            let mut body_policy = policy
                .sanitize(status, &mut HeaderMap::new(), Some(http::Version::HTTP_2))
                .unwrap();
            let mut trailers = HeaderMap::new();
            trailers.insert("x-result", HeaderValue::from_static("must-reject"));

            let error = body_policy
                .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
                .unwrap_err();
            if protocol == HeaderProtocol::Http3 {
                assert!(
                    error.to_string().contains("HTTP/3 response trailers"),
                    "unexpected {protocol:?} {status} trailer error: {error}"
                );
            } else {
                assert!(
                    error.to_string().contains("must not contain trailers"),
                    "unexpected {protocol:?} {status} trailer error: {error}"
                );
            }
        }
    }
}

#[test]
fn h11_response_with_declared_trailers_uses_chunked_framing() {
    let policy = ResponseHeaderPolicy::new(HeaderProtocol::Http11, http::Method::GET, true, None);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
    headers.insert(TRAILER, HeaderValue::from_static("x-checksum"));

    let mut trailer_policy = policy
        .sanitize(http::StatusCode::OK, &mut headers, None)
        .unwrap();

    assert!(!headers.contains_key(CONTENT_LENGTH));
    assert_eq!(headers[TRANSFER_ENCODING], "chunked");
    assert_eq!(headers[TRAILER], "x-checksum");
    trailer_policy
        .sanitize_frame(http_body::Frame::data(bytes::Bytes::from_static(b"body")))
        .unwrap();
    let mut trailers = HeaderMap::new();
    trailers.insert("x-checksum", HeaderValue::from_static("sum"));
    let trailers = trailer_policy
        .sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers))
        .unwrap()
        .unwrap()
        .into_trailers()
        .unwrap();
    assert_eq!(trailers["x-checksum"], "sum");
}

#[test]
fn h10_and_bodyless_responses_remove_trailer_metadata() {
    for (policy, body_allowed) in [
        (
            ResponseHeaderPolicy::new(HeaderProtocol::Http10, http::Method::GET, false, None),
            true,
        ),
        (
            ResponseHeaderPolicy::new(HeaderProtocol::Http11, http::Method::HEAD, true, None),
            false,
        ),
        (
            ResponseHeaderPolicy::new(HeaderProtocol::Http11, http::Method::GET, false, None),
            true,
        ),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
        headers.insert(TRAILER, HeaderValue::from_static("x-checksum"));

        let mut trailer_policy = policy
            .sanitize(http::StatusCode::OK, &mut headers, None)
            .unwrap();

        assert_eq!(headers[CONTENT_LENGTH], "4");
        assert!(!headers.contains_key(TRAILER));
        assert!(!headers.contains_key(TRANSFER_ENCODING));
        if body_allowed {
            trailer_policy
                .sanitize_frame(http_body::Frame::data(bytes::Bytes::from_static(b"body")))
                .unwrap();
        }
        let mut trailers = HeaderMap::new();
        trailers.insert("x-checksum", HeaderValue::from_static("drop"));
        let result =
            trailer_policy.sanitize_frame(http_body::Frame::<bytes::Bytes>::trailers(trailers));
        assert!(matches!(result, Ok(None)), "body allowed: {body_allowed}");
    }
}

#[test]
fn response_body_policy_enforces_content_length_at_data_and_end_stream() {
    let policy = ResponseHeaderPolicy::new(HeaderProtocol::Http2, http::Method::GET, true, None);

    let mut short_headers = HeaderMap::new();
    short_headers.insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
    let mut short = policy
        .sanitize(http::StatusCode::OK, &mut short_headers, None)
        .unwrap();
    short
        .sanitize_frame(http_body::Frame::data(bytes::Bytes::from_static(b"abc")))
        .unwrap();
    assert!(short.finish().is_err());

    let mut long_headers = HeaderMap::new();
    long_headers.insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
    let mut long = policy
        .sanitize(http::StatusCode::OK, &mut long_headers, None)
        .unwrap();
    assert!(
        long.sanitize_frame(http_body::Frame::data(bytes::Bytes::from_static(b"abcde")))
            .is_err()
    );
}

#[test]
fn transfer_encoding_takes_precedence_over_removed_response_content_length() {
    let policy = ResponseHeaderPolicy::new(HeaderProtocol::Http11, http::Method::GET, false, None);
    let mut headers = HeaderMap::new();
    headers.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("3"));

    let mut body = policy
        .sanitize(
            http::StatusCode::OK,
            &mut headers,
            Some(http::Version::HTTP_11),
        )
        .unwrap();

    assert!(!headers.contains_key(TRANSFER_ENCODING));
    assert!(!headers.contains_key(CONTENT_LENGTH));
    body.sanitize_frame(http_body::Frame::data(bytes::Bytes::from_static(b"data")))
        .unwrap();
    body.finish().unwrap();
}
