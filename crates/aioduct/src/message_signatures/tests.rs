use super::*;
use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, DATE, HOST};
use http::{HeaderMap, HeaderValue, Method, Uri};

fn config() -> MessageSignatureConfig {
    MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::authority())
        .component(MessageSignatureComponent::path())
        .component(MessageSignatureComponent::header(CONTENT_LENGTH))
        .component(MessageSignatureComponent::header(CONTENT_TYPE))
        .created(1_618_884_473)
        .key_id("test-key-rsa-pss")
}

#[test]
fn builds_signature_base_for_core_components() {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("18"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let target_uri: Uri = "https://example.com/foo?param=Value&Pet=dog"
        .parse()
        .unwrap();
    let request_target: Uri = "/foo?param=Value&Pet=dog".parse().unwrap();

    let base = config()
        .signature_base(&Method::POST, &target_uri, &request_target, &headers)
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\"@method\": POST\n\
         \"@authority\": example.com\n\
         \"@path\": /foo\n\
         \"content-length\": 18\n\
         \"content-type\": application/json\n\
         \"@signature-params\": (\"@method\" \"@authority\" \"@path\" \"content-length\" \"content-type\");created=1618884473;keyid=\"test-key-rsa-pss\""
    );
}

#[test]
fn method_preserves_case() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method());
    let target_uri: Uri = "https://example.com/".parse().unwrap();
    let request_target: Uri = "/".parse().unwrap();
    let method = Method::from_bytes(b"custom").unwrap();

    let base = cfg
        .signature_base(&method, &target_uri, &request_target, &HeaderMap::new())
        .unwrap();

    assert!(base.as_str().starts_with("\"@method\": custom"));
}

#[test]
fn scheme_lowercases_and_authority_omits_default_port() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::scheme())
        .component(MessageSignatureComponent::authority());
    let target_uri: Uri = "HTTPS://Example.COM:443/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert!(base.as_str().contains("\"@scheme\": https"));
    assert!(base.as_str().contains("\"@authority\": example.com"));
}

#[test]
fn authority_keeps_non_default_port() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::authority());
    let target_uri: Uri = "https://example.com:8443/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert!(base.as_str().contains("\"@authority\": example.com:8443"));
}

#[test]
fn request_target_uses_actual_request_uri() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::target_uri())
        .component(MessageSignatureComponent::request_target());
    let target_uri: Uri = "https://example.com/path?x=1".parse().unwrap();
    let request_target: Uri = "https://example.com/path?x=1".parse().unwrap();

    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert!(
        base.as_str()
            .contains("\"@target-uri\": https://example.com/path?x=1")
    );
    assert!(
        base.as_str()
            .contains("\"@request-target\": https://example.com/path?x=1")
    );
}

#[test]
fn empty_path_is_normalized_to_slash() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::path());
    let target_uri: Uri = "https://example.com".parse().unwrap();
    let request_target: Uri = "/".parse().unwrap();

    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert!(base.as_str().contains("\"@path\": /"));
}

#[test]
fn query_includes_leading_question_mark_and_absent_query_is_question_mark() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::query());
    let target_uri: Uri = "https://example.com/path?a=1".parse().unwrap();
    let request_target: Uri = "/path?a=1".parse().unwrap();
    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();
    assert!(base.as_str().contains("\"@query\": ?a=1"));

    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();
    assert!(base.as_str().contains("\"@query\": ?"));
}

#[test]
fn query_param_uses_form_decoding_and_percent_encoding() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::query_param("var").unwrap())
        .component(MessageSignatureComponent::query_param("bar").unwrap())
        .component(MessageSignatureComponent::query_param("façade\": ").unwrap());
    let target_uri: Uri = concat!(
        "https://example.com/parameters?",
        "var=this%20is%20a%20big%0Amultiline%20value&",
        "bar=with+plus+whitespace&",
        "fa%C3%A7ade%22%3A%20=something"
    )
    .parse()
    .unwrap();
    let request_target: Uri = target_uri
        .path_and_query()
        .unwrap()
        .as_str()
        .parse()
        .unwrap();

    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\
\"@query-param\";name=\"var\": this%20is%20a%20big%0Amultiline%20value\n\
\"@query-param\";name=\"bar\": with%20plus%20whitespace\n\
\"@query-param\";name=\"fa%C3%A7ade%22%3A%20\": something\n\
\"@signature-params\": (\"@query-param\";name=\"var\" \"@query-param\";name=\"bar\" \"@query-param\";name=\"fa%C3%A7ade%22%3A%20\")"
    );
}

#[test]
fn query_param_empty_value_is_signed() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::query_param("qux").unwrap());
    let target_uri: Uri = "https://example.com/path?qux=".parse().unwrap();
    let request_target: Uri = "/path?qux=".parse().unwrap();

    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\"@query-param\";name=\"qux\": \n\"@signature-params\": (\"@query-param\";name=\"qux\")"
    );
}

#[test]
fn query_param_missing_and_duplicate_values_error() {
    let missing_cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::query_param("Pet").unwrap());
    let target_uri: Uri = "https://example.com/path?cat=tabby".parse().unwrap();
    let request_target: Uri = "/path?cat=tabby".parse().unwrap();
    let err = missing_cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap_err();
    assert!(matches!(err, MessageSignatureError::MissingQueryParam(name) if name == "Pet"));

    let duplicate_cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::query_param("Pet").unwrap());
    let target_uri: Uri = "https://example.com/path?Pet=dog&Pet=cat".parse().unwrap();
    let request_target: Uri = "/path?Pet=dog&Pet=cat".parse().unwrap();
    let err = duplicate_cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap_err();
    assert!(matches!(err, MessageSignatureError::DuplicateQueryParam(name) if name == "Pet"));
}

#[test]
fn header_values_are_lowercase_identifiers_and_comma_joined() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(CACHE_CONTROL));
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.append(CACHE_CONTROL, HeaderValue::from_static("max-age=60"));
    headers.append(CACHE_CONTROL, HeaderValue::from_static(" must-revalidate "));

    let base = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert!(
        base.as_str()
            .contains("\"cache-control\": max-age=60, must-revalidate")
    );
}

#[test]
fn missing_header_errors() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(DATE));
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let err = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::MissingHeader(name) if name == DATE));
}

#[test]
fn internal_tab_header_value_is_signed() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(CONTENT_TYPE));
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_bytes(b"application\tjson").unwrap(),
    );

    let base = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert!(
        base.as_str()
            .contains("\"content-type\": application\tjson")
    );
}

#[test]
fn byte_sequence_header_values_are_signed_as_structured_field_list() {
    let name = http::header::HeaderName::from_static("example-header");
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(name.clone()).byte_sequence());
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.append(name.clone(), HeaderValue::from_static("value, with, lots"));
    headers.append(name, HeaderValue::from_static(" of, commas\t"));

    let base = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\"example-header\";bs: :dmFsdWUsIHdpdGgsIGxvdHM=:, :b2YsIGNvbW1hcw==:\n\"@signature-params\": (\"example-header\";bs)"
    );
}

#[test]
fn byte_sequence_parameter_is_only_supported_for_header_fields() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method().byte_sequence());
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let err = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::UnsupportedComponentParameters(component)
            if component == "\"@method\";bs"
    ));
}

#[test]
fn structured_field_header_values_are_signed_with_strict_serialization() {
    let dict = http::header::HeaderName::from_static("example-dict");
    let list = http::header::HeaderName::from_static("example-list");
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(dict.clone()).structured_field())
        .component(MessageSignatureComponent::header(list.clone()).structured_field());
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        dict,
        HeaderValue::from_static(" a=1,    b=2;x=1;y=2,   c=(a   b   c), d "),
    );
    headers.append(list.clone(), HeaderValue::from_static(" 1;foo=bar "));
    headers.append(list, HeaderValue::from_static(" (a   b);q=01.200 "));

    let base = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\
\"example-dict\";sf: a=1, b=2;x=1;y=2, c=(a b c), d\n\
\"example-list\";sf: 1;foo=bar, (a b);q=1.2\n\
\"@signature-params\": (\"example-dict\";sf \"example-list\";sf)"
    );
}

#[test]
fn structured_field_rejects_malformed_values_and_non_header_targets() {
    let name = http::header::HeaderName::from_static("example-dict");
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(name.clone()).structured_field());
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(name.clone(), HeaderValue::from_static("a=(unterminated"));

    let err = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedStructuredField(field) if field == name
    ));

    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method().structured_field());
    let err = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::UnsupportedComponentParameters(component)
            if component == "\"@method\";sf"
    ));
}

#[test]
fn dictionary_key_header_values_are_signed_as_structured_field_members() {
    let name = http::header::HeaderName::from_static("example-dict");
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(
            MessageSignatureComponent::header(name.clone())
                .key("a")
                .unwrap(),
        )
        .component(
            MessageSignatureComponent::header(name.clone())
                .key("d")
                .unwrap(),
        )
        .component(
            MessageSignatureComponent::header(name.clone())
                .key("b")
                .unwrap(),
        )
        .component(
            MessageSignatureComponent::header(name.clone())
                .key("c")
                .unwrap(),
        );
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        name,
        HeaderValue::from_static(" a=1, b=2;x=1;y=2, c=(a   b    c), d "),
    );

    let base = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\
\"example-dict\";key=\"a\": 1\n\
\"example-dict\";key=\"d\": ?1\n\
\"example-dict\";key=\"b\": 2;x=1;y=2\n\
\"example-dict\";key=\"c\": (a b c)\n\
\"@signature-params\": (\"example-dict\";key=\"a\" \"example-dict\";key=\"d\" \"example-dict\";key=\"b\" \"example-dict\";key=\"c\")"
    );
}

#[test]
fn dictionary_key_allows_redundant_structured_field_parameter() {
    let name = http::header::HeaderName::from_static("example-dict");
    let cfg = MessageSignatureConfig::new("sig1").unwrap().component(
        MessageSignatureComponent::header(name.clone())
            .structured_field()
            .key("a")
            .unwrap(),
    );
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(name, HeaderValue::from_static("a=1"));

    let base = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\"example-dict\";sf;key=\"a\": 1\n\"@signature-params\": (\"example-dict\";sf;key=\"a\")"
    );
}

#[test]
fn dictionary_key_missing_malformed_and_duplicate_values() {
    let name = http::header::HeaderName::from_static("example-dict");
    let cfg = MessageSignatureConfig::new("sig1").unwrap().component(
        MessageSignatureComponent::header(name.clone())
            .key("a")
            .unwrap(),
    );
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(name.clone(), HeaderValue::from_static("b=1"));
    let err = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MissingDictionaryKey { field, key }
            if field == name && key == "a"
    ));

    headers.insert(name.clone(), HeaderValue::from_static("a=(unterminated"));
    let err = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedStructuredField(field) if field == name
    ));

    headers.insert(name.clone(), HeaderValue::from_static("a=1, a=2"));
    let base = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();
    assert_eq!(
        base.as_str(),
        "\"example-dict\";key=\"a\": 2\n\"@signature-params\": (\"example-dict\";key=\"a\")"
    );
}

#[test]
fn dictionary_key_rejects_duplicate_member_identities() {
    let name = http::header::HeaderName::from_static("example-dict");
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(
            MessageSignatureComponent::header(name.clone())
                .key("a")
                .unwrap(),
        )
        .component(
            MessageSignatureComponent::header(name)
                .structured_field()
                .key("a")
                .unwrap(),
        );
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let headers = HeaderMap::new();

    let err = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::DuplicateComponent(component)
            if component == "\"example-dict\";sf;key=\"a\""
    ));
}

#[test]
fn dictionary_key_rejects_duplicate_structured_field_parameters() {
    let name = http::header::HeaderName::from_static("example-dict");
    let cfg = MessageSignatureConfig::new("sig1").unwrap().component(
        MessageSignatureComponent::header(name)
            .structured_field()
            .structured_field()
            .key("a")
            .unwrap(),
    );
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let headers = HeaderMap::new();

    let err = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::UnsupportedComponentParameters(component)
            if component == "\"example-dict\";sf;sf;key=\"a\""
    ));
}

#[test]
fn duplicate_components_error() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::method());
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let err = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap_err();

    assert!(
        matches!(err, MessageSignatureError::DuplicateComponent(component) if component == "\"@method\"")
    );
}

#[test]
fn headers_from_signature_requires_components() {
    let err = MessageSignatureConfig::new("sig1")
        .unwrap()
        .headers_from_signature([1_u8, 2, 3])
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::EmptyComponents));
}

#[test]
fn headers_from_signature_rejects_duplicate_components() {
    let err = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::method())
        .headers_from_signature([1_u8, 2, 3])
        .unwrap_err();

    assert!(
        matches!(err, MessageSignatureError::DuplicateComponent(component) if component == "\"@method\"")
    );
}

#[test]
fn structured_fields_integer_parameters_must_be_in_range() {
    let err = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .created(1_000_000_000_000_000)
        .headers_from_signature([1_u8, 2, 3])
        .unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::InvalidIntegerParameter {
            parameter: "created",
            value: 1_000_000_000_000_000,
        }
    ));
}

#[test]
fn invalid_label_errors() {
    let err = MessageSignatureConfig::new("Sig1").unwrap_err();
    assert!(matches!(err, MessageSignatureError::InvalidLabel(label) if label == "Sig1"));
}

#[test]
fn string_parameters_are_escaped() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .key_id("key\\\"1");
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert!(base.as_str().contains(";keyid=\"key\\\\\\\"1\""));
}

#[test]
fn headers_from_signature_formats_structured_fields() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .created(1)
        .algorithm("test");

    let headers = cfg.headers_from_signature([1_u8, 2, 3]).unwrap();

    assert_eq!(
        headers.signature_input.to_str().unwrap(),
        "sig1=(\"@method\");created=1;alg=\"test\""
    );
    assert_eq!(headers.signature.to_str().unwrap(), "sig1=:AQID:");
}

#[test]
fn insert_into_replaces_signature_headers() {
    let generated = MessageSignatureHeaders {
        signature_input: HeaderValue::from_static("sig1=(\"@method\")"),
        signature: HeaderValue::from_static("sig1=:AQID:"),
    };
    let mut headers = HeaderMap::new();
    headers.insert(HOST, HeaderValue::from_static("example.com"));
    headers.insert("signature-input", HeaderValue::from_static("old=()"));
    headers.insert("signature", HeaderValue::from_static("old=:AA:"));

    generated.insert_into(&mut headers);

    assert_eq!(headers["signature-input"], "sig1=(\"@method\")");
    assert_eq!(headers["signature"], "sig1=:AQID:");
    assert_eq!(headers[HOST], "example.com");
}

#[test]
fn sign_request_uses_signer_callback() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method());
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();

    let headers = cfg
        .sign_request(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
            &|base: &[u8]| {
                assert_eq!(
                    std::str::from_utf8(base).unwrap(),
                    "\"@method\": GET\n\"@signature-params\": (\"@method\")"
                );
                Ok(vec![9, 8, 7])
            },
        )
        .unwrap();

    assert_eq!(headers.signature.to_str().unwrap(), "sig1=:CQgH:");
}
