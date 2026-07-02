use super::*;
use std::cell::Cell;

use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, DATE, HOST, HeaderName};
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};

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

fn verification_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"sig1=("@method" "@path" "content-type");created=100;expires=150;keyid="test-key";alg="test-alg""#,
        ),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    headers
}

fn response_verification_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"sig1=("@status" "content-type" "@method";req);created=100;expires=150;keyid="test-key";alg="test-alg""#,
        ),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    headers
}

fn request_digest_verification_headers(content_digest: HeaderValue) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("content-digest", content_digest);
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@method" "content-digest")"#),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    headers
}

fn response_digest_verification_headers(content_digest: HeaderValue) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("content-digest", content_digest);
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@status" "content-digest")"#),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    headers
}

fn accept_verification(
    _: MessageSignatureVerificationInput<'_>,
) -> Result<bool, MessageSignatureError> {
    Ok(true)
}

fn reject_verification(
    _: MessageSignatureVerificationInput<'_>,
) -> Result<bool, MessageSignatureError> {
    Ok(false)
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
            MessageSignatureComponent::header(name.clone())
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

    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(
            MessageSignatureComponent::header(name.clone())
                .key("a")
                .unwrap()
                .related_request(),
        )
        .component(
            MessageSignatureComponent::header(name)
                .structured_field()
                .key("a")
                .unwrap()
                .related_request(),
        );
    let err = cfg
        .request_response_signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &headers,
            StatusCode::OK,
            &headers,
        )
        .unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::DuplicateComponent(component)
            if component == "\"example-dict\";sf;key=\"a\";req"
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
fn empty_covered_component_set_builds_signature_params_only_base() {
    let cfg = MessageSignatureConfig::new("sig-b21")
        .unwrap()
        .created(1_618_884_473)
        .key_id("test-key-rsa-pss")
        .nonce("b3k2pp5k7z-50gnwp.yemd");
    let target_uri: Uri = "https://example.com/foo?param=Value&Pet=dog"
        .parse()
        .unwrap();
    let request_target: Uri = "/foo?param=Value&Pet=dog".parse().unwrap();

    let base = cfg
        .signature_base(
            &Method::POST,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();
    let headers = cfg.headers_from_signature([1_u8, 2, 3]).unwrap();

    assert_eq!(
        base.as_str(),
        r#""@signature-params": ();created=1618884473;nonce="b3k2pp5k7z-50gnwp.yemd";keyid="test-key-rsa-pss""#
    );
    assert_eq!(
        headers.signature_input.to_str().unwrap(),
        r#"sig-b21=();created=1618884473;nonce="b3k2pp5k7z-50gnwp.yemd";keyid="test-key-rsa-pss""#
    );
    assert_eq!(headers.signature.to_str().unwrap(), "sig-b21=:AQID:");
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
fn insert_into_merges_signature_headers_by_label() {
    let generated = MessageSignatureHeaders {
        label: "sig1".to_owned(),
        signature_input: HeaderValue::from_static("sig1=(\"@method\")"),
        signature: HeaderValue::from_static("sig1=:AQID:"),
    };
    let mut headers = HeaderMap::new();
    headers.insert(HOST, HeaderValue::from_static("example.com"));
    headers.insert(
        "signature-input",
        HeaderValue::from_static("old=(), sig1=(\"@path\")"),
    );
    headers.insert(
        "signature",
        HeaderValue::from_static("old=:AA:, sig1=:c3RhbGU:"),
    );

    generated.insert_into(&mut headers).unwrap();

    assert_eq!(headers["signature-input"], "old=(), sig1=(\"@method\")");
    assert_eq!(headers["signature"], "old=:AA==:, sig1=:AQID:");
    assert_eq!(headers[HOST], "example.com");
}

#[test]
fn insert_into_replaces_partial_existing_owned_label() {
    let generated = MessageSignatureHeaders {
        label: "sig1".to_owned(),
        signature_input: HeaderValue::from_static("sig1=(\"@method\")"),
        signature: HeaderValue::from_static("sig1=:AQID:"),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "signature-input",
        HeaderValue::from_static("old=(), sig1=(\"@path\")"),
    );
    headers.insert("signature", HeaderValue::from_static("old=:AA:"));

    generated.insert_into(&mut headers).unwrap();

    assert_eq!(headers["signature-input"], "old=(), sig1=(\"@method\")");
    assert_eq!(headers["signature"], "old=:AA==:, sig1=:AQID:");
}

#[test]
fn insert_into_replaces_duplicate_existing_owned_label() {
    let generated = MessageSignatureHeaders {
        label: "sig1".to_owned(),
        signature_input: HeaderValue::from_static("sig1=(\"@method\")"),
        signature: HeaderValue::from_static("sig1=:AQID:"),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "signature-input",
        HeaderValue::from_static("sig1=(\"@path\"), old=(), sig1=(\"@query\")"),
    );
    headers.insert(
        "signature",
        HeaderValue::from_static("sig1=:c3RhbGU:, old=:AA:, sig1=:b2xk:"),
    );

    generated.insert_into(&mut headers).unwrap();

    assert_eq!(headers["signature-input"], "old=(), sig1=(\"@method\")");
    assert_eq!(headers["signature"], "old=:AA==:, sig1=:AQID:");
}

#[test]
fn insert_into_rejects_invalid_existing_signature_dictionaries() {
    let generated = MessageSignatureHeaders {
        label: "sig1".to_owned(),
        signature_input: HeaderValue::from_static("sig1=(\"@method\")"),
        signature: HeaderValue::from_static("sig1=:AQID:"),
    };

    let mut malformed = HeaderMap::new();
    malformed.insert("signature-input", HeaderValue::from_static("old=("));
    malformed.insert("signature", HeaderValue::from_static("old=:AA:"));
    let err = generated.clone().insert_into(&mut malformed).unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedSignatureHeader("signature-input")
    ));

    let mut duplicate = HeaderMap::new();
    duplicate.insert(
        "signature-input",
        HeaderValue::from_static("old=(), old=()"),
    );
    duplicate.insert("signature", HeaderValue::from_static("old=:AA:"));
    let err = generated.clone().insert_into(&mut duplicate).unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::DuplicateSignatureLabel { header, label }
            if header == "signature-input" && label == "old"
    ));

    let mut mismatched = HeaderMap::new();
    mismatched.insert("signature-input", HeaderValue::from_static("old=()"));
    mismatched.insert("signature", HeaderValue::from_static("other=:AA:"));
    let err = generated.insert_into(&mut mismatched).unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MismatchedSignatureLabels
    ));
}

#[test]
fn parsed_signature_selects_label_and_rebuilds_request_base() {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"old=("@method");created=1, sig1=("@method" "@path" "content-type");created=2;expires=3;keyid="test-key";ext="preserved""#,
        ),
    );
    headers.insert(
        "signature",
        HeaderValue::from_static("old=:b2xk:, sig1=:AQID:"),
    );
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();

    let signature = MessageSignature::from_headers(&headers, "sig1").unwrap();
    let base = signature
        .signature_base(&Method::POST, &target_uri, &request_target, &headers)
        .unwrap();

    assert_eq!(signature.label(), "sig1");
    assert_eq!(signature.signature(), &[1, 2, 3]);
    assert_eq!(signature.params().created(), Some(2));
    assert_eq!(signature.params().expires(), Some(3));
    assert_eq!(signature.params().key_id(), Some("test-key"));
    assert_eq!(signature.params().algorithm(), None);
    assert_eq!(signature.components().len(), 3);
    assert_eq!(
        signature.signature_params_value(),
        r#"("@method" "@path" "content-type");created=2;expires=3;keyid="test-key";ext="preserved""#
    );
    assert_eq!(
        base.as_str(),
        "\
\"@method\": POST\n\
\"@path\": /foo\n\
\"content-type\": application/json\n\
\"@signature-params\": (\"@method\" \"@path\" \"content-type\");created=2;expires=3;keyid=\"test-key\";ext=\"preserved\""
    );
}

#[test]
fn parsed_signature_handles_component_parameters() {
    let dict = http::header::HeaderName::from_static("example-dict");
    let bin = http::header::HeaderName::from_static("x-bin");
    let mut headers = HeaderMap::new();
    headers.insert(dict.clone(), HeaderValue::from_static("a=1, b=2"));
    headers.insert(bin.clone(), HeaderValue::from_static(" value\t"));
    headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"sig1=("@query-param";name="Pet" "example-dict";key="a" "x-bin";bs);created=1;nonce="n";alg="test";tag="api""#,
        ),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:AA:"));
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();

    let signature = MessageSignature::from_headers(&headers, "sig1").unwrap();
    let base = signature
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert_eq!(signature.params().created(), Some(1));
    assert_eq!(signature.params().nonce(), Some("n"));
    assert_eq!(signature.params().algorithm(), Some("test"));
    assert_eq!(signature.params().tag(), Some("api"));
    assert_eq!(
        base.as_str(),
        "\
\"@query-param\";name=\"Pet\": dog\n\
\"example-dict\";key=\"a\": 1\n\
\"x-bin\";bs: :dmFsdWU=:\n\
\"@signature-params\": (\"@query-param\";name=\"Pet\" \"example-dict\";key=\"a\" \"x-bin\";bs);created=1;nonce=\"n\";alg=\"test\";tag=\"api\""
    );
}

#[test]
fn parsed_signature_accepts_empty_covered_set() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"sig-b21=();created=1618884473;keyid="test-key-rsa-pss";nonce="b3k2pp5k7z-50gnwp.yemd""#,
        ),
    );
    headers.insert("signature", HeaderValue::from_static("sig-b21=::"));
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();

    let signature = MessageSignature::from_headers(&headers, "sig-b21").unwrap();
    let base = signature
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap();

    assert!(signature.components().is_empty());
    assert_eq!(signature.params().created(), Some(1_618_884_473));
    assert_eq!(signature.params().key_id(), Some("test-key-rsa-pss"));
    assert_eq!(signature.params().nonce(), Some("b3k2pp5k7z-50gnwp.yemd"));
    assert_eq!(signature.signature(), &[]);
    assert_eq!(
        signature.signature_params_value(),
        r#"();created=1618884473;keyid="test-key-rsa-pss";nonce="b3k2pp5k7z-50gnwp.yemd""#
    );
    assert_eq!(
        base.as_str(),
        r#""@signature-params": ();created=1618884473;keyid="test-key-rsa-pss";nonce="b3k2pp5k7z-50gnwp.yemd""#
    );
}

#[test]
fn parsed_signature_reports_selection_and_header_errors() {
    let mut missing = HeaderMap::new();
    missing.insert("signature-input", HeaderValue::from_static("sig1=()"));
    missing.insert("signature", HeaderValue::from_static("sig1=::"));
    let err = MessageSignature::from_headers(&missing, "sig2").unwrap_err();
    assert!(matches!(err, MessageSignatureError::MissingSignatureLabel(label) if label == "sig2"));

    let mut mismatched = HeaderMap::new();
    mismatched.insert("signature-input", HeaderValue::from_static("sig1=()"));
    mismatched.insert("signature", HeaderValue::from_static("sig2=::"));
    let err = MessageSignature::from_headers(&mismatched, "sig1").unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MismatchedSignatureLabels
    ));

    let mut duplicate = HeaderMap::new();
    duplicate.insert(
        "signature-input",
        HeaderValue::from_static("sig1=(), sig1=()"),
    );
    duplicate.insert("signature", HeaderValue::from_static("sig1=::"));
    let err = MessageSignature::from_headers(&duplicate, "sig1").unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::DuplicateSignatureLabel { header, label }
            if header == "signature-input" && label == "sig1"
    ));

    let mut malformed_input = HeaderMap::new();
    malformed_input.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1="not-list""#),
    );
    malformed_input.insert("signature", HeaderValue::from_static("sig1=::"));
    let err = MessageSignature::from_headers(&malformed_input, "sig1").unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedSignatureHeader("signature-input")
    ));

    let mut malformed_signature = HeaderMap::new();
    malformed_signature.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@method")"#),
    );
    malformed_signature.insert("signature", HeaderValue::from_static(r#"sig1="not-bytes""#));
    let err = MessageSignature::from_headers(&malformed_signature, "sig1").unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedSignatureHeader("signature")
    ));
}

#[test]
fn parsed_signature_rejects_signature_params_component() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@signature-params")"#),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=::"));

    let err = MessageSignature::from_headers(&headers, "sig1").unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::UnsupportedComponent(component)
            if component == "\"@signature-params\""
    ));
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

#[test]
fn builds_response_signature_base_for_status_and_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::header(CONTENT_TYPE))
        .created(1_618_884_479);

    let base = cfg
        .response_signature_base(StatusCode::SERVICE_UNAVAILABLE, &headers)
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\
\"@status\": 503\n\
\"content-type\": application/json\n\
\"@signature-params\": (\"@status\" \"content-type\");created=1618884479"
    );
}

#[test]
fn response_context_uses_caller_supplied_trailer_fields() {
    let trailer_name = HeaderName::from_static("trailer");
    let expires = HeaderName::from_static("expires");
    let mut headers = HeaderMap::new();
    headers.insert(trailer_name.clone(), HeaderValue::from_static("Expires"));
    let mut trailers = HeaderMap::new();
    trailers.insert(
        expires.clone(),
        HeaderValue::from_static("Wed, 9 Nov 2022 07:28:00 GMT"),
    );
    let response =
        MessageSignatureResponseContext::new(StatusCode::OK, &headers).with_trailers(&trailers);
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::header(trailer_name))
        .component(MessageSignatureComponent::header(expires).trailer());

    let signature_headers = cfg
        .sign_response_context(response, &|base: &[u8]| {
            assert_eq!(
                std::str::from_utf8(base).unwrap(),
                "\
\"@status\": 200\n\
\"trailer\": Expires\n\
\"expires\";tr: Wed, 9 Nov 2022 07:28:00 GMT\n\
\"@signature-params\": (\"@status\" \"trailer\" \"expires\";tr)"
            );
            Ok(vec![9, 8, 7])
        })
        .unwrap();

    assert_eq!(
        signature_headers.signature_input.to_str().unwrap(),
        r#"sig1=("@status" "trailer" "expires";tr)"#
    );
    assert_eq!(signature_headers.signature.to_str().unwrap(), "sig1=:CQgH:");
}

#[test]
fn trailer_fields_are_distinct_from_headers_and_support_field_parameters() {
    let dict = HeaderName::from_static("example-dict");
    let list = HeaderName::from_static("example-list");
    let raw = HeaderName::from_static("example-raw");
    let mut headers = HeaderMap::new();
    headers.insert(dict.clone(), HeaderValue::from_static("a=1"));
    let mut trailers = HeaderMap::new();
    trailers.insert(dict.clone(), HeaderValue::from_static("a=2;x=1"));
    trailers.append(list.clone(), HeaderValue::from_static(" 1;foo=bar "));
    trailers.append(list.clone(), HeaderValue::from_static(" (a   b);q=01.200 "));
    trailers.insert(
        raw.clone(),
        HeaderValue::from_static(" value, with, lots\t"),
    );
    let method = Method::GET;
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let request =
        MessageSignatureRequestContext::new(&method, &target_uri, &request_target, &headers)
            .with_trailers(&trailers);
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(
            MessageSignatureComponent::header(dict.clone())
                .key("a")
                .unwrap(),
        )
        .component(
            MessageSignatureComponent::header(dict)
                .trailer()
                .key("a")
                .unwrap(),
        )
        .component(
            MessageSignatureComponent::header(list)
                .trailer()
                .structured_field(),
        )
        .component(
            MessageSignatureComponent::header(raw)
                .trailer()
                .byte_sequence(),
        );

    let base = cfg.signature_base_for_request_context(request).unwrap();

    assert_eq!(
        base.as_str(),
        "\
\"example-dict\";key=\"a\": 1\n\
\"example-dict\";tr;key=\"a\": 2;x=1\n\
\"example-list\";tr;sf: 1;foo=bar, (a b);q=1.2\n\
\"example-raw\";tr;bs: :dmFsdWUsIHdpdGgsIGxvdHM=:\n\
\"@signature-params\": (\"example-dict\";key=\"a\" \"example-dict\";tr;key=\"a\" \"example-list\";tr;sf \"example-raw\";tr;bs)"
    );
}

#[test]
fn trailer_components_require_attached_trailer_fields() {
    let expires = HeaderName::from_static("expires");
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::header(expires).trailer());
    let headers = HeaderMap::new();
    let response = MessageSignatureResponseContext::new(StatusCode::OK, &headers);

    let err = cfg
        .response_signature_base_for_context(response)
        .unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::ComponentNotAvailable {
            context: "trailers",
            ..
        }
    ));
}

#[test]
fn request_response_signature_base_uses_related_request_components() {
    let mut request_headers = HeaderMap::new();
    request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::header(CONTENT_TYPE))
        .component(MessageSignatureComponent::method().related_request())
        .component(MessageSignatureComponent::path().related_request())
        .component(MessageSignatureComponent::header(CONTENT_TYPE).related_request());

    let base = cfg
        .request_response_signature_base(
            &Method::POST,
            &target_uri,
            &request_target,
            &request_headers,
            StatusCode::SERVICE_UNAVAILABLE,
            &response_headers,
        )
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\
\"@status\": 503\n\
\"content-type\": application/problem+json\n\
\"@method\";req: POST\n\
\"@path\";req: /foo\n\
\"content-type\";req: application/json\n\
\"@signature-params\": (\"@status\" \"content-type\" \"@method\";req \"@path\";req \"content-type\";req)"
    );
}

#[test]
fn response_signature_rejects_components_from_wrong_context() {
    let response_headers = HeaderMap::new();
    let method = Method::GET;
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request_headers = HeaderMap::new();

    let err = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .response_signature_base(StatusCode::OK, &response_headers)
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::ComponentNotAvailable {
            context: "response",
            ..
        }
    ));

    let err = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method().related_request())
        .response_signature_base(StatusCode::OK, &response_headers)
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::ComponentNotAvailable {
            context: "response",
            ..
        }
    ));

    let err = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status().related_request())
        .request_response_signature_base(
            &method,
            &target_uri,
            &request_target,
            &request_headers,
            StatusCode::OK,
            &response_headers,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::UnsupportedComponentParameters(component)
            if component == "\"@status\";req"
    ));
}

#[test]
fn parsed_signature_rebuilds_response_and_related_request_base() {
    let mut request_headers = HeaderMap::new();
    request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    response_headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"sig1=("@status" "content-type" "@method";req "content-type";req);created=2"#,
        ),
    );
    response_headers.insert("signature", HeaderValue::from_static("sig1=:AQID:"));
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();

    let signature = MessageSignature::from_headers(&response_headers, "sig1").unwrap();
    let base = signature
        .request_response_signature_base(
            &Method::POST,
            &target_uri,
            &request_target,
            &request_headers,
            StatusCode::SERVICE_UNAVAILABLE,
            &response_headers,
        )
        .unwrap();

    assert_eq!(
        base.as_str(),
        "\
\"@status\": 503\n\
\"content-type\": application/problem+json\n\
\"@method\";req: POST\n\
\"content-type\";req: application/json\n\
\"@signature-params\": (\"@status\" \"content-type\" \"@method\";req \"content-type\";req);created=2"
    );
}

#[test]
fn sign_response_uses_signer_callback() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status());

    let headers = cfg
        .sign_response(StatusCode::OK, &HeaderMap::new(), &|base: &[u8]| {
            assert_eq!(
                std::str::from_utf8(base).unwrap(),
                "\"@status\": 200\n\"@signature-params\": (\"@status\")"
            );
            Ok(vec![9, 8, 7])
        })
        .unwrap();

    assert_eq!(headers.signature.to_str().unwrap(), "sig1=:CQgH:");
}

#[test]
fn accept_signature_parses_rfc_style_request() {
    let accept = AcceptSignature::parse(
        r#"sig1=("@method" "@target-uri" "@authority" "content-digest" "cache-control");keyid="test-key-rsa-pss";created;tag="app-123""#,
    )
    .unwrap();

    assert_eq!(accept.entries().len(), 1);
    let entry = &accept.entries()[0];
    assert_eq!(entry.label(), "sig1");
    assert_eq!(entry.components().len(), 5);
    assert_eq!(entry.params().key_id(), Some("test-key-rsa-pss"));
    assert!(entry.params().created_requested());
    assert!(!entry.params().expires_requested());
    assert_eq!(entry.params().tag(), Some("app-123"));
    entry.validate_request_target().unwrap();
    assert!(matches!(
        entry.validate_response_target().unwrap_err(),
        MessageSignatureError::ComponentNotAvailable {
            context: "response",
            ..
        }
    ));
}

#[test]
fn accept_signature_formats_and_inserts_header() {
    let accept = AcceptSignature::new().entry(
        AcceptSignatureEntry::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::status())
            .component(MessageSignatureComponent::method().related_request())
            .created()
            .key_id("test-key"),
    );

    let value = accept.header_value().unwrap();
    assert_eq!(
        value.to_str().unwrap(),
        r#"sig1=("@status" "@method";req);created;keyid="test-key""#
    );
    accept.validate_request_response_target().unwrap();

    let mut headers = HeaderMap::new();
    accept.insert_into(&mut headers).unwrap();
    assert_eq!(headers["accept-signature"], value);
}

#[test]
fn accept_signature_from_headers_combines_field_values() {
    let mut headers = HeaderMap::new();
    headers.append(
        "accept-signature",
        HeaderValue::from_static(r#"sig1=("@status")"#),
    );
    headers.append(
        "accept-signature",
        HeaderValue::from_static(r#"sig2=("content-type");expires"#),
    );

    let accept = AcceptSignature::from_headers(&headers).unwrap();

    assert_eq!(accept.entries().len(), 2);
    assert_eq!(accept.entries()[0].label(), "sig1");
    assert_eq!(accept.entries()[1].label(), "sig2");
    assert!(accept.entries()[1].params().expires_requested());
    accept.validate_response_target().unwrap();
}

#[test]
fn accept_signature_allows_empty_covered_component_request() {
    let accept = AcceptSignature::parse(r#"sig1=();created;keyid="test-key""#).unwrap();
    let fulfillment = AcceptSignatureFulfillment::new().created(100);
    let configs = accept.request_signature_configs(&fulfillment).unwrap();
    let cfg = &configs[0];
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();

    assert!(accept.entries()[0].components().is_empty());
    accept.validate_request_target().unwrap();
    let base = cfg
        .signature_base(
            &Method::GET,
            &target_uri,
            &request_target,
            &HeaderMap::new(),
        )
        .unwrap();

    assert_eq!(cfg.components(), &[]);
    assert_eq!(
        base.as_str(),
        r#""@signature-params": ();created=100;keyid="test-key""#
    );
    assert_eq!(
        cfg.headers_from_signature([1_u8, 2, 3])
            .unwrap()
            .signature_input
            .to_str()
            .unwrap(),
        r#"sig1=();created=100;keyid="test-key""#
    );
}

#[test]
fn accept_signature_reports_header_errors() {
    let err = AcceptSignature::parse(r#"sig1=("@method");created=100"#).unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedSignatureHeader("accept-signature")
    ));

    let err = AcceptSignature::parse(r#"sig1=("@method");unknown="dropped""#).unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedSignatureHeader("accept-signature")
    ));

    let err = AcceptSignature::parse(r#"sig1=("@method"), sig1=("@path")"#).unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::DuplicateSignatureLabel { header, label }
            if header == "accept-signature" && label == "sig1"
    ));

    let err = AcceptSignature::new()
        .entry(
            AcceptSignatureEntry::new("sig1")
                .unwrap()
                .component(MessageSignatureComponent::method()),
        )
        .entry(
            AcceptSignatureEntry::new("sig1")
                .unwrap()
                .component(MessageSignatureComponent::path()),
        )
        .header_value()
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::DuplicateSignatureLabel { header, label }
            if header == "accept-signature" && label == "sig1"
    ));
}

#[test]
fn accept_signature_validates_target_message_components() {
    let request = AcceptSignatureEntry::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::method());
    request.validate_request_target().unwrap();
    assert!(matches!(
        request.validate_request_response_target().unwrap_err(),
        MessageSignatureError::ComponentNotAvailable {
            context: "response",
            ..
        }
    ));

    let response = AcceptSignatureEntry::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status());
    response.validate_response_target().unwrap();
    assert!(matches!(
        response.validate_request_target().unwrap_err(),
        MessageSignatureError::ComponentNotAvailable {
            context: "request",
            ..
        }
    ));

    let related_request_response = AcceptSignatureEntry::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::method().related_request());
    related_request_response
        .validate_request_response_target()
        .unwrap();
    assert!(matches!(
        related_request_response
            .validate_response_target()
            .unwrap_err(),
        MessageSignatureError::ComponentNotAvailable {
            context: "response",
            ..
        }
    ));
}

#[test]
fn accept_signature_fulfills_response_with_related_request() {
    let accept = AcceptSignature::parse(
        r#"sig1=("@status" "content-type" "@method";req);created;nonce="server-nonce";alg="test-alg";keyid="server-key""#,
    )
    .unwrap();
    let fulfillment = AcceptSignatureFulfillment::new()
        .created(100)
        .algorithm("test-alg")
        .key_id("server-key");
    let configs = accept
        .request_response_signature_configs(&fulfillment)
        .unwrap();
    let config = &configs[0];
    let target_uri: Uri = "https://example.com/api".parse().unwrap();
    let request_target: Uri = "/api".parse().unwrap();
    let request_headers = HeaderMap::new();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let base = config
        .request_response_signature_base(
            &Method::POST,
            &target_uri,
            &request_target,
            &request_headers,
            StatusCode::CREATED,
            &response_headers,
        )
        .unwrap();
    assert_eq!(
        base.as_str(),
        "\
\"@status\": 201\n\
\"content-type\": application/json\n\
\"@method\";req: POST\n\
\"@signature-params\": (\"@status\" \"content-type\" \"@method\";req);created=100;nonce=\"server-nonce\";alg=\"test-alg\";keyid=\"server-key\""
    );

    config
        .headers_from_signature([9, 8, 7])
        .unwrap()
        .insert_into(&mut response_headers)
        .unwrap();
    assert_eq!(
        response_headers["signature-input"],
        r#"sig1=("@status" "content-type" "@method";req);created=100;nonce="server-nonce";alg="test-alg";keyid="server-key""#,
    );
    assert_eq!(response_headers["signature"], "sig1=:CQgH:");
}

#[test]
fn accept_signature_fulfills_next_request() {
    let accept =
        AcceptSignature::parse(r#"sig1=("@method" "@path");created;keyid="client-key";tag="next""#)
            .unwrap();
    let fulfillment = AcceptSignatureFulfillment::new()
        .created(200)
        .key_id("client-key");
    let configs = accept.request_signature_configs(&fulfillment).unwrap();
    let config = &configs[0];
    let target_uri: Uri = "https://example.com/next".parse().unwrap();
    let request_target: Uri = "/next".parse().unwrap();
    let mut headers = HeaderMap::new();

    config
        .sign_request(
            &Method::GET,
            &target_uri,
            &request_target,
            &headers,
            &|base: &[u8]| {
                assert_eq!(
                    std::str::from_utf8(base).unwrap(),
                    "\
\"@method\": GET\n\
\"@path\": /next\n\
\"@signature-params\": (\"@method\" \"@path\");created=200;keyid=\"client-key\";tag=\"next\""
                );
                Ok(vec![1, 2, 3])
            },
        )
        .unwrap()
        .insert_into(&mut headers)
        .unwrap();

    assert_eq!(
        headers["signature-input"],
        r#"sig1=("@method" "@path");created=200;keyid="client-key";tag="next""#,
    );
    assert_eq!(headers["signature"], "sig1=:AQID:");
}

#[test]
fn accept_signature_fulfillment_reports_unfulfillable_requests() {
    let missing_created = AcceptSignature::parse(r#"sig1=("@method");created"#).unwrap();
    let err = missing_created
        .request_signature_configs(&AcceptSignatureFulfillment::new())
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::UnfulfillableAcceptSignatureParameter("created")
    ));

    let conflicting_algorithm =
        AcceptSignature::parse(r#"sig1=("@method");alg="ed25519""#).unwrap();
    let err = conflicting_algorithm
        .request_signature_configs(&AcceptSignatureFulfillment::new().algorithm("rsa-pss-sha512"))
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::UnfulfillableAcceptSignatureParameter("alg")
    ));

    let response_only = AcceptSignature::parse(r#"sig1=("@status")"#).unwrap();
    let err = response_only
        .request_signature_configs(&AcceptSignatureFulfillment::new())
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::ComponentNotAvailable {
            context: "request",
            ..
        }
    ));
}

#[test]
fn accept_signature_allows_ignoring_requests_and_adding_signatures() {
    let accept =
        AcceptSignature::parse(r#"ignored=("@status"), good=("@method");created"#).unwrap();
    let fulfillment = AcceptSignatureFulfillment::new().created(2);
    let target_uri: Uri = "https://example.com/next".parse().unwrap();
    let request_target: Uri = "/next".parse().unwrap();
    let mut headers = HeaderMap::new();

    MessageSignatureConfig::new("extra")
        .unwrap()
        .component(MessageSignatureComponent::path())
        .created(1)
        .sign_request(
            &Method::GET,
            &target_uri,
            &request_target,
            &headers,
            &|_: &[u8]| Ok(vec![1]),
        )
        .unwrap()
        .insert_into(&mut headers)
        .unwrap();

    accept.entries()[1]
        .request_signature_config(&fulfillment)
        .unwrap()
        .sign_request(
            &Method::GET,
            &target_uri,
            &request_target,
            &headers,
            &|_: &[u8]| Ok(vec![2]),
        )
        .unwrap()
        .insert_into(&mut headers)
        .unwrap();

    assert_eq!(
        headers["signature-input"],
        r#"extra=("@path");created=1, good=("@method");created=2"#,
    );
    assert_eq!(headers["signature"], "extra=:AQ==:, good=:Ag==:");
}

#[test]
fn verification_policy_calls_verifier_with_response_base() {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        "signature-input",
        HeaderValue::from_static(
            r#"sig1=("@status" "content-type");created=100;expires=150;keyid="test-key";alg="test-alg""#,
        ),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    let response = MessageSignatureResponseContext::new(StatusCode::SERVICE_UNAVAILABLE, &headers);
    let policy = MessageSignatureVerificationPolicy::new()
        .required_component(MessageSignatureComponent::status())
        .required_component(MessageSignatureComponent::header(CONTENT_TYPE))
        .accepted_algorithm("test-alg")
        .accepted_key_id("test-key")
        .validation_time(125)
        .max_age(60);

    policy
        .verify_response(
            response,
            "sig1",
            &|input: MessageSignatureVerificationInput<'_>| {
                assert_eq!(input.label(), "sig1");
                assert_eq!(input.signature(), &[9, 8, 7]);
                assert_eq!(
                    std::str::from_utf8(input.signature_base()).unwrap(),
                    "\
\"@status\": 503\n\
\"content-type\": application/json\n\
\"@signature-params\": (\"@status\" \"content-type\");created=100;expires=150;keyid=\"test-key\";alg=\"test-alg\""
                );
                Ok(true)
            },
        )
        .unwrap();
}

#[test]
fn verification_policy_calls_verifier_with_related_request_response_base() {
    let mut request_headers = HeaderMap::new();
    request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let response_headers = response_verification_headers();
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();
    let request = MessageSignatureRequestContext::new(
        &Method::POST,
        &target_uri,
        &request_target,
        &request_headers,
    );
    let response = MessageSignatureResponseContext::new(StatusCode::BAD_GATEWAY, &response_headers);
    let policy = MessageSignatureVerificationPolicy::new()
        .required_component(MessageSignatureComponent::status())
        .required_component(MessageSignatureComponent::method().related_request())
        .accepted_algorithm("test-alg")
        .accepted_key_id("test-key")
        .validation_time(125);

    policy
        .verify_request_response(
            request,
            response,
            "sig1",
            &|input: MessageSignatureVerificationInput<'_>| {
                assert_eq!(input.params().created(), Some(100));
                assert_eq!(
                    std::str::from_utf8(input.signature_base()).unwrap(),
                    "\
\"@status\": 502\n\
\"content-type\": application/problem+json\n\
\"@method\";req: POST\n\
\"@signature-params\": (\"@status\" \"content-type\" \"@method\";req);created=100;expires=150;keyid=\"test-key\";alg=\"test-alg\""
                );
                Ok(true)
            },
        )
        .unwrap();
}

#[test]
fn verification_policy_calls_verifier_with_trailer_components() {
    let expires = HeaderName::from_static("expires");
    let request_headers = HeaderMap::new();
    let mut request_trailers = HeaderMap::new();
    request_trailers.insert(
        expires.clone(),
        HeaderValue::from_static("Wed, 9 Nov 2022 07:29:00 GMT"),
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@status" "expires";tr "expires";tr;req)"#),
    );
    response_headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    let mut response_trailers = HeaderMap::new();
    response_trailers.insert(
        expires,
        HeaderValue::from_static("Wed, 9 Nov 2022 07:28:00 GMT"),
    );
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request = MessageSignatureRequestContext::new(
        &Method::POST,
        &target_uri,
        &request_target,
        &request_headers,
    )
    .with_trailers(&request_trailers);
    let response = MessageSignatureResponseContext::new(StatusCode::OK, &response_headers)
        .with_trailers(&response_trailers);
    let verifier_calls = Cell::new(0);

    MessageSignatureVerificationPolicy::new()
        .verify_request_response(
            request,
            response,
            "sig1",
            &|input: MessageSignatureVerificationInput<'_>| {
                verifier_calls.set(verifier_calls.get() + 1);
                assert_eq!(
                    std::str::from_utf8(input.signature_base()).unwrap(),
                    "\
\"@status\": 200\n\
\"expires\";tr: Wed, 9 Nov 2022 07:28:00 GMT\n\
\"expires\";tr;req: Wed, 9 Nov 2022 07:29:00 GMT\n\
\"@signature-params\": (\"@status\" \"expires\";tr \"expires\";tr;req)"
                );
                Ok(true)
            },
        )
        .unwrap();

    assert_eq!(verifier_calls.get(), 1);
}

#[test]
fn parsed_response_signature_can_verify_with_policy() {
    let response_headers = response_verification_headers();
    let response = MessageSignatureResponseContext::new(StatusCode::OK, &response_headers);
    let signature = MessageSignature::from_headers(&response_headers, "sig1").unwrap();
    let policy = MessageSignatureVerificationPolicy::new()
        .accepted_algorithm("test-alg")
        .accepted_key_id("test-key")
        .validation_time(125);

    let err = signature
        .verify_response(&policy, response, &accept_verification)
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::ComponentNotAvailable {
            context: "response",
            ..
        }
    ));

    let mut request_headers = HeaderMap::new();
    request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();
    let request = MessageSignatureRequestContext::new(
        &Method::POST,
        &target_uri,
        &request_target,
        &request_headers,
    );

    signature
        .verify_request_response(&policy, request, response, &accept_verification)
        .unwrap();
}

#[test]
fn verification_policy_calls_verifier_with_rebuilt_base() {
    let headers = verification_headers();
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();
    let policy = MessageSignatureVerificationPolicy::new()
        .required_component(MessageSignatureComponent::method())
        .required_component(MessageSignatureComponent::header(CONTENT_TYPE))
        .accepted_algorithm("test-alg")
        .accepted_key_id("test-key")
        .validation_time(125)
        .max_age(60);

    policy
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &|input: MessageSignatureVerificationInput<'_>| {
                assert_eq!(input.label(), "sig1");
                assert_eq!(input.params().created(), Some(100));
                assert_eq!(input.params().expires(), Some(150));
                assert_eq!(input.params().algorithm(), Some("test-alg"));
                assert_eq!(input.params().key_id(), Some("test-key"));
                assert_eq!(input.signature(), &[9, 8, 7]);
                assert_eq!(
                    std::str::from_utf8(input.signature_base()).unwrap(),
                    "\
\"@method\": POST\n\
\"@path\": /foo\n\
\"content-type\": application/json\n\
\"@signature-params\": (\"@method\" \"@path\" \"content-type\");created=100;expires=150;keyid=\"test-key\";alg=\"test-alg\""
                );
                Ok(true)
            },
        )
        .unwrap();
}

#[test]
fn verification_policy_allows_empty_covered_component_set() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=();created=100;keyid="test-key""#),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();
    let policy = MessageSignatureVerificationPolicy::new()
        .accepted_key_id("test-key")
        .validation_time(125);

    policy
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &|input: MessageSignatureVerificationInput<'_>| {
                assert_eq!(input.label(), "sig1");
                assert_eq!(input.params().created(), Some(100));
                assert_eq!(input.params().key_id(), Some("test-key"));
                assert_eq!(input.signature(), &[9, 8, 7]);
                assert_eq!(
                    std::str::from_utf8(input.signature_base()).unwrap(),
                    r#""@signature-params": ();created=100;keyid="test-key""#
                );
                Ok(true)
            },
        )
        .unwrap();
}

#[test]
fn verification_policy_checks_request_content_digest_before_signature() {
    let headers = request_digest_verification_headers(HeaderValue::from_static(
        "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:",
    ));
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request =
        MessageSignatureRequestContext::new(&Method::POST, &target_uri, &request_target, &headers)
            .with_body(b"hello");
    let verifier_calls = Cell::new(0);

    MessageSignatureVerificationPolicy::new()
        .required_component(MessageSignatureComponent::header(HeaderName::from_static(
            "content-digest",
        )))
        .verify_request_context(
            request,
            "sig1",
            &|input: MessageSignatureVerificationInput<'_>| {
                verifier_calls.set(verifier_calls.get() + 1);
                assert_eq!(input.signature(), &[9, 8, 7]);
                assert!(
                    std::str::from_utf8(input.signature_base())
                        .unwrap()
                        .contains(r#""content-digest": sha-256=:"#)
                );
                Ok(true)
            },
        )
        .unwrap();

    assert_eq!(verifier_calls.get(), 1);
}

#[test]
fn verification_policy_rejects_mismatched_request_content_digest_before_signature() {
    let headers = request_digest_verification_headers(HeaderValue::from_static(
        "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:",
    ));
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request =
        MessageSignatureRequestContext::new(&Method::POST, &target_uri, &request_target, &headers)
            .with_body(b"goodbye");
    let verifier_calls = Cell::new(0);

    let err = MessageSignatureVerificationPolicy::new()
        .verify_request_context(request, "sig1", &|_: MessageSignatureVerificationInput<
            '_,
        >| {
            verifier_calls.set(verifier_calls.get() + 1);
            Ok(true)
        })
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::ContentDigestMismatch));
    assert_eq!(verifier_calls.get(), 0);
}

#[test]
fn verification_policy_checks_sha256_content_digest_dictionary_member_before_signature() {
    let mut headers = request_digest_verification_headers(HeaderValue::from_static(
        "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:, sha-512=:AQID:",
    ));
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@method" "content-digest";key="sha-256")"#),
    );
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request =
        MessageSignatureRequestContext::new(&Method::POST, &target_uri, &request_target, &headers)
            .with_body(b"goodbye");
    let verifier_calls = Cell::new(0);

    let err = MessageSignatureVerificationPolicy::new()
        .verify_request_context(request, "sig1", &|_: MessageSignatureVerificationInput<
            '_,
        >| {
            verifier_calls.set(verifier_calls.get() + 1);
            Ok(true)
        })
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::ContentDigestMismatch));
    assert_eq!(verifier_calls.get(), 0);
}

#[test]
fn verification_policy_skips_content_digest_member_that_is_not_covered() {
    let mut headers = request_digest_verification_headers(HeaderValue::from_static(
        "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:, sha-512=:AQID:",
    ));
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@method" "content-digest";key="sha-512")"#),
    );
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request =
        MessageSignatureRequestContext::new(&Method::POST, &target_uri, &request_target, &headers)
            .with_body(b"goodbye");
    let verifier_calls = Cell::new(0);

    MessageSignatureVerificationPolicy::new()
        .verify_request_context(request, "sig1", &|_: MessageSignatureVerificationInput<
            '_,
        >| {
            verifier_calls.set(verifier_calls.get() + 1);
            Ok(true)
        })
        .unwrap();

    assert_eq!(verifier_calls.get(), 1);
}

#[test]
fn verification_policy_rejects_malformed_and_unsupported_content_digest_before_signature() {
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();

    for (content_digest, expected) in [
        (
            HeaderValue::from_static("sha-256=123"),
            MessageSignatureError::MalformedContentDigest,
        ),
        (
            HeaderValue::from_static("sha-512=:AQID:"),
            MessageSignatureError::UnsupportedContentDigestAlgorithm,
        ),
    ] {
        let headers = request_digest_verification_headers(content_digest);
        let request = MessageSignatureRequestContext::new(
            &Method::POST,
            &target_uri,
            &request_target,
            &headers,
        )
        .with_body(b"hello");
        let verifier_calls = Cell::new(0);
        let err = MessageSignatureVerificationPolicy::new()
            .verify_request_context(request, "sig1", &|_: MessageSignatureVerificationInput<
                '_,
            >| {
                verifier_calls.set(verifier_calls.get() + 1);
                Ok(true)
            })
            .unwrap_err();

        assert_eq!(
            std::mem::discriminant(&err),
            std::mem::discriminant(&expected)
        );
        assert_eq!(verifier_calls.get(), 0);
    }
}

#[test]
fn verification_policy_checks_response_content_digest_before_signature() {
    let headers = response_digest_verification_headers(HeaderValue::from_static(
        "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:",
    ));
    let response =
        MessageSignatureResponseContext::new(StatusCode::OK, &headers).with_body(b"hello");
    let verifier_calls = Cell::new(0);

    MessageSignatureVerificationPolicy::new()
        .verify_response(
            response,
            "sig1",
            &|input: MessageSignatureVerificationInput<'_>| {
                verifier_calls.set(verifier_calls.get() + 1);
                assert!(
                    std::str::from_utf8(input.signature_base())
                        .unwrap()
                        .contains(r#""@status": 200"#)
                );
                Ok(true)
            },
        )
        .unwrap();

    assert_eq!(verifier_calls.get(), 1);
}

#[test]
fn verification_policy_checks_request_trailer_content_digest_before_signature() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@method" "content-digest";tr)"#),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "content-digest",
        HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
    );
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request =
        MessageSignatureRequestContext::new(&Method::POST, &target_uri, &request_target, &headers)
            .with_trailers(&trailers)
            .with_body(b"goodbye");
    let verifier_calls = Cell::new(0);

    let err = MessageSignatureVerificationPolicy::new()
        .verify_request_context(request, "sig1", &|_: MessageSignatureVerificationInput<
            '_,
        >| {
            verifier_calls.set(verifier_calls.get() + 1);
            Ok(true)
        })
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::ContentDigestMismatch));
    assert_eq!(verifier_calls.get(), 0);
}

#[test]
fn verification_policy_checks_response_trailer_content_digest_before_signature() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@status" "content-digest";tr)"#),
    );
    headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "content-digest",
        HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
    );
    let response = MessageSignatureResponseContext::new(StatusCode::OK, &headers)
        .with_trailers(&trailers)
        .with_body(b"goodbye");
    let verifier_calls = Cell::new(0);

    let err = MessageSignatureVerificationPolicy::new()
        .verify_response(response, "sig1", &|_: MessageSignatureVerificationInput<
            '_,
        >| {
            verifier_calls.set(verifier_calls.get() + 1);
            Ok(true)
        })
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::ContentDigestMismatch));
    assert_eq!(verifier_calls.get(), 0);
}

#[test]
fn verification_policy_checks_related_request_content_digest_before_signature() {
    let mut request_headers = HeaderMap::new();
    request_headers.insert(
        "content-digest",
        HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "content-digest",
        HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
    );
    response_headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@status" "content-digest" "content-digest";req)"#),
    );
    response_headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request = MessageSignatureRequestContext::new(
        &Method::POST,
        &target_uri,
        &request_target,
        &request_headers,
    )
    .with_body(b"goodbye");
    let response =
        MessageSignatureResponseContext::new(StatusCode::OK, &response_headers).with_body(b"hello");
    let verifier_calls = Cell::new(0);

    let err = MessageSignatureVerificationPolicy::new()
        .verify_request_response(
            request,
            response,
            "sig1",
            &|_: MessageSignatureVerificationInput<'_>| {
                verifier_calls.set(verifier_calls.get() + 1);
                Ok(true)
            },
        )
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::ContentDigestMismatch));
    assert_eq!(verifier_calls.get(), 0);
}

#[test]
fn verification_policy_checks_related_request_trailer_content_digest_before_signature() {
    let request_headers = HeaderMap::new();
    let mut request_trailers = HeaderMap::new();
    request_trailers.insert(
        "content-digest",
        HeaderValue::from_static("sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:"),
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@status" "content-digest";tr;req)"#),
    );
    response_headers.insert("signature", HeaderValue::from_static("sig1=:CQgH:"));
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let request = MessageSignatureRequestContext::new(
        &Method::POST,
        &target_uri,
        &request_target,
        &request_headers,
    )
    .with_trailers(&request_trailers)
    .with_body(b"goodbye");
    let response = MessageSignatureResponseContext::new(StatusCode::OK, &response_headers);
    let verifier_calls = Cell::new(0);

    let err = MessageSignatureVerificationPolicy::new()
        .verify_request_response(
            request,
            response,
            "sig1",
            &|_: MessageSignatureVerificationInput<'_>| {
                verifier_calls.set(verifier_calls.get() + 1);
                Ok(true)
            },
        )
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::ContentDigestMismatch));
    assert_eq!(verifier_calls.get(), 0);
}

#[test]
fn verification_policy_skips_content_digest_check_when_body_is_unavailable() {
    let headers = request_digest_verification_headers(HeaderValue::from_static(
        "sha-256=:LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=:",
    ));
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let verifier_calls = Cell::new(0);

    MessageSignatureVerificationPolicy::new()
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &|_: MessageSignatureVerificationInput<'_>| {
                verifier_calls.set(verifier_calls.get() + 1);
                Ok(true)
            },
        )
        .unwrap();

    assert_eq!(verifier_calls.get(), 1);
}

#[test]
fn parsed_signature_can_verify_with_policy() {
    let headers = verification_headers();
    let target_uri: Uri = "https://example.com/foo?Pet=dog".parse().unwrap();
    let request_target: Uri = "/foo?Pet=dog".parse().unwrap();
    let signature = MessageSignature::from_headers(&headers, "sig1").unwrap();
    let policy = MessageSignatureVerificationPolicy::new()
        .accepted_algorithm("test-alg")
        .accepted_key_id("test-key")
        .validation_time(125);

    signature
        .verify_request(
            &policy,
            &Method::POST,
            &target_uri,
            &request_target,
            &headers,
            &accept_verification,
        )
        .unwrap();
}

#[test]
fn verification_policy_reports_selection_and_header_errors() {
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let policy = MessageSignatureVerificationPolicy::new();

    let headers = verification_headers();
    let err = policy
        .verify_request(
            &headers,
            "sig2",
            &Method::GET,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(err, MessageSignatureError::MissingSignatureLabel(label) if label == "sig2"));

    let mut malformed = HeaderMap::new();
    malformed.insert("signature-input", HeaderValue::from_static("sig1=("));
    malformed.insert("signature", HeaderValue::from_static("sig1=:AA:"));
    let err = policy
        .verify_request(
            &malformed,
            "sig1",
            &Method::GET,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MalformedSignatureHeader("signature-input")
    ));

    let mut duplicate = HeaderMap::new();
    duplicate.insert(
        "signature-input",
        HeaderValue::from_static(r#"sig1=("@method"), sig1=("@path")"#),
    );
    duplicate.insert("signature", HeaderValue::from_static("sig1=:AA:"));
    let err = policy
        .verify_request(
            &duplicate,
            "sig1",
            &Method::GET,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::DuplicateSignatureLabel { header, label }
            if header == "signature-input" && label == "sig1"
    ));
}

#[test]
fn verification_policy_rejects_unacceptable_signature_metadata() {
    let headers = verification_headers();
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();

    let err = MessageSignatureVerificationPolicy::new()
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(err, MessageSignatureError::MissingValidationTime));

    let err = MessageSignatureVerificationPolicy::new()
        .required_component(MessageSignatureComponent::authority())
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::MissingRequiredComponent(component)
            if component == "\"@authority\""
    ));

    let err = MessageSignatureVerificationPolicy::new()
        .accepted_algorithm("other")
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::UnacceptableAlgorithm(Some(algorithm))
            if algorithm == "test-alg"
    ));

    let err = MessageSignatureVerificationPolicy::new()
        .accepted_key_id("other")
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::UnknownKeyId(Some(key_id)) if key_id == "test-key"
    ));

    let err = MessageSignatureVerificationPolicy::new()
        .validation_time(151)
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::SignatureExpired {
            expires: 150,
            now: 151
        }
    ));

    let err = MessageSignatureVerificationPolicy::new()
        .validation_time(125)
        .max_age(20)
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &accept_verification,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MessageSignatureError::SignatureTooOld {
            created: 100,
            now: 125,
            max_age: 20,
        }
    ));
}

#[test]
fn verification_policy_rejects_failed_verifier_callback() {
    let headers = verification_headers();
    let target_uri: Uri = "https://example.com/foo".parse().unwrap();
    let request_target: Uri = "/foo".parse().unwrap();
    let err = MessageSignatureVerificationPolicy::new()
        .validation_time(125)
        .verify_request(
            &headers,
            "sig1",
            &Method::POST,
            &target_uri,
            &request_target,
            &reject_verification,
        )
        .unwrap_err();

    assert!(matches!(err, MessageSignatureError::VerificationFailed));
}
