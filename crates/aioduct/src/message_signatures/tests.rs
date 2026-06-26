use super::*;
use http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, DATE, HOST};
use http::{HeaderMap, HeaderValue, Method, Uri};

fn config() -> MessageSignatureConfig {
    MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::Method)
        .component(MessageSignatureComponent::Authority)
        .component(MessageSignatureComponent::Path)
        .component(MessageSignatureComponent::Header {
            name: CONTENT_LENGTH,
        })
        .component(MessageSignatureComponent::Header { name: CONTENT_TYPE })
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
        .component(MessageSignatureComponent::Method);
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
        .component(MessageSignatureComponent::Scheme)
        .component(MessageSignatureComponent::Authority);
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
        .component(MessageSignatureComponent::Authority);
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
        .component(MessageSignatureComponent::TargetUri)
        .component(MessageSignatureComponent::RequestTarget);
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
        .component(MessageSignatureComponent::Path);
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
        .component(MessageSignatureComponent::Query);
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
fn header_values_are_lowercase_identifiers_and_comma_joined() {
    let cfg =
        MessageSignatureConfig::new("sig1")
            .unwrap()
            .component(MessageSignatureComponent::Header {
                name: CACHE_CONTROL,
            });
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
        .component(MessageSignatureComponent::Header { name: DATE });
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
fn control_character_header_value_errors() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::Header { name: CONTENT_TYPE });
    let target_uri: Uri = "https://example.com/path".parse().unwrap();
    let request_target: Uri = "/path".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_bytes(b"application\tjson").unwrap(),
    );

    let err = cfg
        .signature_base(&Method::GET, &target_uri, &request_target, &headers)
        .unwrap_err();

    assert!(matches!(
        err,
        MessageSignatureError::ControlCharacterInComponentValue
    ));
}

#[test]
fn duplicate_components_error() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::Method)
        .component(MessageSignatureComponent::Method);
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
fn invalid_label_errors() {
    let err = MessageSignatureConfig::new("Sig1").unwrap_err();
    assert!(matches!(err, MessageSignatureError::InvalidLabel(label) if label == "Sig1"));
}

#[test]
fn string_parameters_are_escaped() {
    let cfg = MessageSignatureConfig::new("sig1")
        .unwrap()
        .component(MessageSignatureComponent::Method)
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
        .component(MessageSignatureComponent::Method)
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
        .component(MessageSignatureComponent::Method);
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
