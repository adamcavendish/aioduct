use super::*;

fn parts(method: http::Method, uri: &str, version: http::Version) -> http::request::Parts {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .version(version);
    if version == http::Version::HTTP_11 {
        let authority = uri
            .parse::<http::Uri>()
            .ok()
            .and_then(|uri| uri.authority().cloned())
            .map(|authority| authority.to_string())
            .unwrap_or_else(|| "downstream.test".to_owned());
        builder = builder.header(http::header::HOST, authority);
    }
    let (parts, ()) = builder.body(()).unwrap().into_parts();
    parts
}

fn finalize(
    parts: &mut http::request::Parts,
    hint: ProtocolHint,
    version_changed: bool,
) -> Result<ForwardDispatchPlan, Error> {
    let downstream_connect_protocol = capture_downstream_connect_protocol(parts)?;
    finalize_after_hook(
        parts,
        hint,
        version_changed,
        downstream_connect_protocol.as_deref(),
    )
}

fn finalize_after_hook(
    parts: &mut http::request::Parts,
    hint: ProtocolHint,
    version_changed: bool,
    downstream_connect_protocol: Option<&str>,
) -> Result<ForwardDispatchPlan, Error> {
    let rewritten = "https://example.test/base?q=1".parse().unwrap();
    let downstream_version = parts.version;
    let downstream_method = parts.method.clone();
    let downstream_h1_upgrade_offer = hop_by_hop::h1_upgrade_offer(&parts.headers);
    let inbound_target = InboundRequestTarget::capture(parts)?;
    let plan = ForwardDispatchPlan::finalize(
        parts,
        &rewritten,
        &inbound_target,
        &TrailerPolicy::default(),
        hint,
        false,
        downstream_h1_upgrade_offer,
        version_changed,
        true,
        true,
        downstream_connect_protocol,
        downstream_version,
        &downstream_method,
        true,
        false,
    )?;
    plan.apply(parts, true)?;
    Ok(plan)
}

#[test]
fn rewrite_forward_header_restores_values_without_duplication() {
    let mut parts = parts(
        http::Method::POST,
        "http://downstream.test/upload",
        http::Version::HTTP_11,
    );
    parts.headers.append(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer first"),
    );
    parts.headers.append(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer second"),
    );
    let upstream = "https://upstream.test/base".parse().unwrap();

    rewrite_for_upstream(
        &mut parts,
        ForwardRewrite {
            upstream: &upstream,
            strip_prefix: None,
            preserve_host: false,
            forward_headers: &[http::header::AUTHORIZATION],
            extra_headers: &HeaderMap::new(),
            remove_headers: &[],
        },
    )
    .unwrap();

    let values = parts
        .headers
        .get_all(http::header::AUTHORIZATION)
        .iter()
        .map(HeaderValue::as_bytes)
        .collect::<Vec<_>>();
    assert_eq!(values, [b"Bearer first".as_slice(), b"Bearer second"]);
}

#[test]
fn rewrite_canonicalizes_mixed_case_upstream_scheme() {
    let mut parts = parts(
        http::Method::GET,
        "http://downstream.test/resource",
        http::Version::HTTP_11,
    );
    let upstream = Uri::builder()
        .scheme("HtTpS")
        .authority("upstream.test")
        .path_and_query("/base")
        .build()
        .unwrap();

    let rewritten = rewrite_for_upstream(
        &mut parts,
        ForwardRewrite {
            upstream: &upstream,
            strip_prefix: None,
            preserve_host: false,
            forward_headers: &[],
            extra_headers: &HeaderMap::new(),
            remove_headers: &[],
        },
    )
    .unwrap();

    assert_eq!(rewritten.uri.scheme(), Some(&Scheme::HTTPS));
    assert_eq!(parts.uri.scheme(), Some(&Scheme::HTTPS));
}

#[test]
fn finalization_canonicalizes_mixed_case_hook_schemes() {
    for (raw_scheme, expected_scheme, expected_hint) in [
        ("HTTP", Scheme::HTTP, ProtocolHint::H2c),
        ("HtTpS", Scheme::HTTPS, ProtocolHint::Http2),
    ] {
        let mut parts = parts(
            http::Method::GET,
            "https://downstream.test/resource",
            http::Version::HTTP_2,
        );
        let downstream_version = parts.version;
        let downstream_method = parts.method.clone();
        let inbound_target = InboundRequestTarget::capture(&parts).unwrap();
        let rewritten = "https://upstream.test/base".parse().unwrap();
        parts.uri = Uri::builder()
            .scheme(raw_scheme)
            .authority("hook.test")
            .path_and_query("/hook")
            .build()
            .unwrap();

        let plan = ForwardDispatchPlan::finalize(
            &mut parts,
            &rewritten,
            &inbound_target,
            &TrailerPolicy::default(),
            ProtocolHint::Http2,
            false,
            None,
            false,
            true,
            true,
            None,
            downstream_version,
            &downstream_method,
            true,
            false,
        )
        .unwrap();
        assert_eq!(plan.protocol_hint(), expected_hint, "{raw_scheme}");
        plan.apply(&mut parts, true).unwrap();
        assert_eq!(parts.uri.scheme(), Some(&expected_scheme), "{raw_scheme}");
    }
}

#[test]
fn rewrite_forward_header_cannot_restore_connection_nominated_fields() {
    let mut parts = parts(
        http::Method::POST,
        "http://downstream.test/upload",
        http::Version::HTTP_11,
    );
    parts.headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("x-private"),
    );
    parts
        .headers
        .insert("x-private", HeaderValue::from_static("secret"));
    let upstream = "https://upstream.test/base".parse().unwrap();

    rewrite_for_upstream(
        &mut parts,
        ForwardRewrite {
            upstream: &upstream,
            strip_prefix: None,
            preserve_host: false,
            forward_headers: &[http::header::HeaderName::from_static("x-private")],
            extra_headers: &HeaderMap::new(),
            remove_headers: &[],
        },
    )
    .unwrap();

    assert!(!parts.headers.contains_key(http::header::CONNECTION));
    assert!(!parts.headers.contains_key("x-private"));
}

#[test]
fn rewrite_can_replace_a_connection_nominated_field_with_a_generated_value() {
    let mut parts = parts(
        http::Method::POST,
        "http://downstream.test/upload",
        http::Version::HTTP_11,
    );
    parts.headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("authorization"),
    );
    parts.headers.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer downstream"),
    );
    let mut generated = HeaderMap::new();
    generated.insert(
        http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer broker"),
    );
    let upstream = "https://upstream.test/base".parse().unwrap();

    rewrite_for_upstream(
        &mut parts,
        ForwardRewrite {
            upstream: &upstream,
            strip_prefix: None,
            preserve_host: false,
            forward_headers: &[http::header::AUTHORIZATION],
            extra_headers: &generated,
            remove_headers: &[],
        },
    )
    .unwrap();

    assert_eq!(parts.headers[http::header::AUTHORIZATION], "Bearer broker");
}

#[test]
fn ingress_versions_canonicalize_for_negotiated_egress() {
    for version in [
        http::Version::HTTP_10,
        http::Version::HTTP_11,
        http::Version::HTTP_2,
        http::Version::HTTP_3,
    ] {
        let mut parts = parts(http::Method::GET, "https://example.test/base", version);
        let plan = finalize(&mut parts, ProtocolHint::Auto, false).unwrap();
        assert_eq!(
            egress_for_hint(plan.protocol_hint),
            EgressProtocol::Negotiated
        );
        assert_eq!(parts.version, http::Version::HTTP_11);
        assert_eq!(parts.uri, "/base");
    }
}

#[test]
fn explicit_hook_version_selects_exact_egress() {
    for (version, egress, hint) in [
        (
            http::Version::HTTP_11,
            EgressProtocol::Http1,
            ProtocolHint::Http1,
        ),
        (
            http::Version::HTTP_2,
            EgressProtocol::Http2,
            ProtocolHint::Http2,
        ),
        (
            http::Version::HTTP_3,
            EgressProtocol::Http3,
            ProtocolHint::Http3,
        ),
    ] {
        let mut parts = parts(http::Method::GET, "https://example.test/base", version);
        let plan = finalize(&mut parts, ProtocolHint::Auto, true).unwrap();
        assert_eq!(egress_for_hint(plan.protocol_hint), egress);
        assert_eq!(plan.protocol_hint, hint);
    }
}

#[test]
fn upgrade_and_extended_connect_require_exact_protocols() {
    let mut upgrade = parts(
        http::Method::GET,
        "https://example.test/base",
        http::Version::HTTP_11,
    );
    upgrade.headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("upgrade"),
    );
    upgrade
        .headers
        .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
    let upgrade_plan = finalize(&mut upgrade, ProtocolHint::Auto, false).unwrap();
    assert_eq!(
        egress_for_hint(upgrade_plan.protocol_hint),
        EgressProtocol::Http1
    );

    let mut extended = parts(
        http::Method::CONNECT,
        "https://example.test/tunnel",
        http::Version::HTTP_2,
    );
    extended
        .extensions
        .insert(crate::Protocol::from_static("websocket"));
    let extended_plan = finalize(&mut extended, ProtocolHint::Auto, false).unwrap();
    assert_eq!(
        egress_for_hint(extended_plan.protocol_hint),
        EgressProtocol::Http2
    );
    assert_eq!(extended.uri, "https://example.test/tunnel");
}

#[test]
fn final_sanitization_restores_the_canonical_wire_host() {
    let mut request = parts(
        http::Method::GET,
        "https://example.test/chat",
        http::Version::HTTP_11,
    );
    request.headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("upgrade, host"),
    );
    request
        .headers
        .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));

    finalize(&mut request, ProtocolHint::Auto, false).unwrap();

    assert_eq!(request.headers[http::header::HOST], "example.test");
    assert_eq!(request.headers[http::header::CONNECTION], "upgrade");
    assert_eq!(request.headers[http::header::UPGRADE], "websocket");
}

#[test]
fn ordinary_connect_uses_authority_form() {
    let mut connect = parts(
        http::Method::CONNECT,
        "example.test:443",
        http::Version::HTTP_11,
    );
    let plan = finalize(&mut connect, ProtocolHint::Auto, false).unwrap();
    assert_eq!(connect.uri, "example.test:443");
    assert_eq!(plan.protocol_hint(), ProtocolHint::Http1);
    assert_eq!(connect.version, http::Version::HTTP_11);
}

#[test]
fn invalid_protocol_combinations_fail() {
    let mut h2_upgrade = parts(
        http::Method::GET,
        "https://example.test/base",
        http::Version::HTTP_11,
    );
    h2_upgrade.headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("upgrade"),
    );
    h2_upgrade
        .headers
        .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
    assert!(finalize(&mut h2_upgrade, ProtocolHint::Http2, false).is_err());

    let mut invalid_extension = parts(
        http::Method::GET,
        "https://example.test/base",
        http::Version::HTTP_2,
    );
    invalid_extension
        .extensions
        .insert(crate::Protocol::from_static("websocket"));
    assert!(finalize(&mut invalid_extension, ProtocolHint::Auto, false).is_err());
}

#[test]
fn extended_connect_protocol_must_be_a_non_empty_token() {
    for protocol in ["", "two words", "protocol/1", "one,two"] {
        let mut request = parts(
            http::Method::CONNECT,
            "https://example.test/tunnel",
            http::Version::HTTP_2,
        );
        request
            .extensions
            .insert(crate::Protocol::from_static(protocol));

        let error = finalize(&mut request, ProtocolHint::Http2, false)
            .err()
            .unwrap();
        assert!(
            matches!(error, Error::InvalidHeader(ref message) if message.contains("non-empty HTTP token")),
            "accepted protocol {protocol:?}: {error}"
        );
    }
}

#[test]
fn hooks_cannot_create_remove_or_change_extended_connect_protocol() {
    let mut created = parts(
        http::Method::CONNECT,
        "https://example.test/tunnel",
        http::Version::HTTP_2,
    );
    let downstream = capture_downstream_connect_protocol(&mut created).unwrap();
    created
        .extensions
        .insert(crate::Protocol::from_static("websocket"));
    let error = finalize_after_hook(
        &mut created,
        ProtocolHint::Http2,
        false,
        downstream.as_deref(),
    )
    .err()
    .unwrap();
    assert!(matches!(error, Error::Unsupported(ref message) if message.contains("cannot create")));

    for replacement in [None, Some("connect-udp")] {
        let mut changed = parts(
            http::Method::CONNECT,
            "https://example.test/tunnel",
            http::Version::HTTP_2,
        );
        changed
            .extensions
            .insert(crate::Protocol::from_static("websocket"));
        let downstream = capture_downstream_connect_protocol(&mut changed).unwrap();
        changed.extensions.remove::<crate::Protocol>();
        if let Some(replacement) = replacement {
            changed
                .extensions
                .insert(crate::Protocol::from_static(replacement));
        }

        let error = finalize_after_hook(
            &mut changed,
            ProtocolHint::Http2,
            false,
            downstream.as_deref(),
        )
        .err()
        .unwrap();
        let expected = if replacement.is_some() {
            "cannot change"
        } else {
            "cannot remove"
        };
        assert!(
            matches!(error, Error::Unsupported(ref message) if message.contains(expected)),
            "unexpected protocol rewrite error: {error}"
        );
    }
}

#[cfg(feature = "http3")]
#[test]
fn h3_extended_connect_protocol_is_rejected() {
    let mut request = parts(
        http::Method::CONNECT,
        "https://example.test/tunnel",
        http::Version::HTTP_3,
    );
    request.extensions.insert(h3::ext::Protocol::CONNECT_UDP);

    let error = capture_downstream_connect_protocol(&mut request).unwrap_err();
    assert!(
        matches!(error, Error::Unsupported(ref message) if message.contains("HTTP/3 extended CONNECT")),
        "{error}"
    );
}

#[test]
fn http10_upgrade_is_rejected() {
    let mut upgrade = parts(
        http::Method::GET,
        "https://example.test/base",
        http::Version::HTTP_10,
    );
    upgrade.headers.insert(
        http::header::CONNECTION,
        HeaderValue::from_static("upgrade"),
    );
    upgrade
        .headers
        .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));

    assert!(finalize(&mut upgrade, ProtocolHint::Auto, false).is_err());
}

#[test]
fn options_asterisk_uses_protocol_specific_wire_targets() {
    let mut negotiated = parts(http::Method::OPTIONS, "*", http::Version::HTTP_11);
    let plan = finalize(&mut negotiated, ProtocolHint::Auto, false).unwrap();
    assert_eq!(plan.protocol_hint(), ProtocolHint::Http1);
    assert_eq!(negotiated.uri, "*");
    assert_eq!(negotiated.version, http::Version::HTTP_11);
    assert!(
        negotiated
            .extensions
            .get::<DeferredForwardTarget>()
            .is_none()
    );

    let mut h2 = parts(http::Method::OPTIONS, "*", http::Version::HTTP_11);
    let error = finalize(&mut h2, ProtocolHint::Http2, false).err().unwrap();
    assert!(
        matches!(error, Error::Unsupported(ref message) if message.contains("cannot be represented by the HTTP/2 transport")),
        "{error}"
    );

    let mut h3 = parts(http::Method::OPTIONS, "*", http::Version::HTTP_11);
    let error = finalize(&mut h3, ProtocolHint::Http3, false).err().unwrap();
    assert!(
        matches!(error, Error::Unsupported(ref message) if message.contains("cannot encode authority-free OPTIONS *")),
        "{error}"
    );
}

#[test]
fn rewrite_exposes_server_wide_options_provenance_to_hooks() {
    fn hook_uri(target: Uri, version: http::Version) -> Uri {
        let mut builder = http::Request::builder()
            .method(http::Method::OPTIONS)
            .uri(target)
            .version(version);
        if version == http::Version::HTTP_11 {
            builder = builder.header(http::header::HOST, "downstream.test");
        }
        let request = builder.body(()).unwrap();
        let (mut parts, ()) = request.into_parts();
        let upstream: Uri = "https://upstream.test/base".parse().unwrap();
        rewrite_for_upstream(
            &mut parts,
            ForwardRewrite {
                upstream: &upstream,
                strip_prefix: None,
                preserve_host: false,
                forward_headers: &[],
                extra_headers: &HeaderMap::new(),
                remove_headers: &[],
            },
        )
        .unwrap();
        parts.uri
    }

    assert_eq!(hook_uri(Uri::from_static("*"), http::Version::HTTP_11), "*");

    let pathless_absolute = Uri::builder()
        .scheme("http")
        .authority("downstream.test")
        .path_and_query("*")
        .build()
        .unwrap();
    let exposed = hook_uri(pathless_absolute, http::Version::HTTP_2);
    assert_eq!(exposed.scheme_str(), Some("https"));
    assert_eq!(exposed.authority().unwrap(), "upstream.test");
    assert_eq!(exposed.path_and_query().unwrap(), "*");
}

#[test]
fn pathless_absolute_options_retains_authority_on_h2_and_h3() {
    for hint in [ProtocolHint::Http2, ProtocolHint::Http3] {
        let mut parts = parts(http::Method::OPTIONS, "/", http::Version::HTTP_11);
        let inbound_target = InboundRequestTarget::Absolute {
            authority: "downstream.test".parse().unwrap(),
            scheme: Scheme::HTTPS,
            server_wide_options: true,
        };
        let rewritten: Uri = "https://example.test/base".parse().unwrap();
        parts.uri = Uri::builder()
            .scheme("https")
            .authority("example.test")
            .path_and_query("*")
            .build()
            .unwrap();
        parts
            .headers
            .insert(http::header::HOST, HeaderValue::from_static("example.test"));

        let plan = ForwardDispatchPlan::finalize(
            &mut parts,
            &rewritten,
            &inbound_target,
            &TrailerPolicy::default(),
            hint,
            false,
            None,
            false,
            true,
            true,
            None,
            http::Version::HTTP_11,
            &http::Method::OPTIONS,
            true,
            false,
        )
        .unwrap();
        plan.apply(&mut parts, true).unwrap();

        assert_eq!(parts.uri.scheme_str(), Some("https"));
        assert_eq!(parts.uri.authority().unwrap(), "example.test");
        assert_eq!(parts.uri.path_and_query().unwrap(), "*");
        assert_eq!(
            parts.version,
            if hint == ProtocolHint::Http2 {
                http::Version::HTTP_2
            } else {
                http::Version::HTTP_3
            }
        );
        assert_eq!(
            parts.extensions.get::<ForwardAsteriskAuthority>(),
            (hint == ProtocolHint::Http3).then_some(&ForwardAsteriskAuthority::Include)
        );
    }
}

#[test]
fn hook_request_targets_are_revalidated_without_losing_target_form() {
    let rewritten: Uri = "https://example.test/base".parse().unwrap();

    let malformed = "relative";
    let hook_uri: Uri = malformed.parse().unwrap();
    assert!(
        resolve_hook_uri(&rewritten, &hook_uri, &http::Method::OPTIONS,).is_err(),
        "accepted hook target {malformed}"
    );

    let (_, literal) =
        resolve_hook_uri(&rewritten, &Uri::from_static("*"), &http::Method::OPTIONS).unwrap();
    assert_eq!(
        literal,
        Some(ServerWideOptions {
            authority_in_target: false
        })
    );

    let absolute = Uri::builder()
        .scheme("https")
        .authority("hook.test")
        .path_and_query("*")
        .build()
        .unwrap();
    let (absolute, server_wide) =
        resolve_hook_uri(&rewritten, &absolute, &http::Method::OPTIONS).unwrap();
    assert_eq!(absolute.path_and_query().unwrap(), "*");
    assert_eq!(
        server_wide,
        Some(ServerWideOptions {
            authority_in_target: true
        })
    );
}

#[test]
fn hook_absolute_targets_normalize_only_an_empty_non_options_path() {
    let rewritten: Uri = "https://example.test/base".parse().unwrap();

    let query_only = Uri::builder()
        .scheme("https")
        .authority("hook.test")
        .path_and_query("?x=1")
        .build()
        .unwrap();
    let (resolved, server_wide) =
        resolve_hook_uri(&rewritten, &query_only, &http::Method::GET).unwrap();
    assert_eq!(resolved, "https://hook.test/?x=1");
    assert_eq!(server_wide, None);
}

#[test]
fn final_empty_host_is_rewritten_or_rejected_by_preserve_host() {
    let fallback = "upstream.test:8443".parse().unwrap();
    let mut rewritten = HeaderMap::new();
    rewritten.insert(http::header::HOST, HeaderValue::from_static(""));
    assert_eq!(
        forwarded_authority(&mut rewritten, &fallback, false, None).unwrap(),
        fallback
    );
    assert_eq!(rewritten[http::header::HOST], "upstream.test:8443");

    let mut preserved = HeaderMap::new();
    preserved.insert(http::header::HOST, HeaderValue::from_static(""));
    assert!(forwarded_authority(&mut preserved, &fallback, true, None).is_err());
    assert_eq!(preserved[http::header::HOST], "");
}

#[test]
fn method_rewrites_preserve_head_and_connect_semantic_classes() {
    for (downstream, upstream, allowed) in [
        (http::Method::GET, http::Method::POST, true),
        (http::Method::HEAD, http::Method::HEAD, true),
        (http::Method::CONNECT, http::Method::CONNECT, true),
        (http::Method::GET, http::Method::HEAD, false),
        (http::Method::HEAD, http::Method::GET, false),
        (http::Method::GET, http::Method::CONNECT, false),
        (http::Method::CONNECT, http::Method::GET, false),
    ] {
        assert_eq!(
            validate_method_rewrite(&downstream, &upstream).is_ok(),
            allowed,
            "{downstream} -> {upstream}"
        );
    }
}

#[test]
fn http11_request_framing_preserves_streamed_data_and_declared_trailers() {
    let mut unknown_length = HeaderMap::new();
    apply_http11_request_framing(&mut unknown_length, true, false).unwrap();
    assert_eq!(unknown_length[http::header::TRANSFER_ENCODING], "chunked");

    let mut fixed_length = HeaderMap::new();
    fixed_length.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("4"));
    apply_http11_request_framing(&mut fixed_length, true, false).unwrap();
    assert_eq!(fixed_length[http::header::CONTENT_LENGTH], "4");
    assert!(!fixed_length.contains_key(http::header::TRANSFER_ENCODING));

    fixed_length.insert(
        http::header::TRAILER,
        HeaderValue::from_static("x-checksum"),
    );
    apply_http11_request_framing(&mut fixed_length, true, true).unwrap();
    assert!(!fixed_length.contains_key(http::header::CONTENT_LENGTH));
    assert_eq!(fixed_length[http::header::TRANSFER_ENCODING], "chunked");
}

#[test]
fn end_stream_request_framing_removes_trailer_declarations() {
    for version in [
        http::Version::HTTP_11,
        http::Version::HTTP_2,
        http::Version::HTTP_3,
    ] {
        let framing = DeferredForwardFraming {
            has_body: false,
            has_trailer_declaration: true,
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::TRAILER,
            HeaderValue::from_static("x-checksum"),
        );
        headers.insert(
            http::header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );

        framing.apply(&mut headers, version).unwrap();

        assert!(!headers.contains_key(http::header::TRAILER));
        assert!(!headers.contains_key(http::header::TRANSFER_ENCODING));
    }
}

#[test]
fn negotiated_h2_framing_removes_http1_transfer_encoding() {
    let framing = DeferredForwardFraming {
        has_body: true,
        has_trailer_declaration: true,
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::TRANSFER_ENCODING,
        HeaderValue::from_static("chunked"),
    );

    framing.apply(&mut headers, http::Version::HTTP_2).unwrap();

    assert!(!headers.contains_key(http::header::TRANSFER_ENCODING));
}
