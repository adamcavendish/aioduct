use base64::Engine as _;
use http::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, DATE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};

use super::{MessageSignatureComponent, MessageSignatureConfig};

const TEST_REQUEST_URI: &str = "https://example.com/foo?param=Value&Pet=dog";
const TEST_REQUEST_TARGET: &str = "/foo?param=Value&Pet=dog";
const REQUEST_CONTENT_DIGEST: &str = concat!(
    "sha-512=:WZDPaVn/7XgHaAy8pmojAkGWoRx2UFChF41A2svX",
    "+TaPm+AbwAgBWnrIiYllu7BNNyealdVLvRwEmTHWXvJwew==:"
);
const CLIENT_CERT: &str = concat!(
    ":MIIBqDCCAU6gAwIBAgIBBzAKBggqhkjOPQQDAjA6MRswGQYDVQQK",
    "DBJMZXQncyBBdXRoZW50aWNhdGUxGzAZBgNVBAMMEkxBIEludGVybWVkaWF0ZSBD",
    "QTAeFw0yMDAxMTQyMjU1MzNaFw0yMTAxMjMyMjU1MzNaMA0xCzAJBgNVBAMMAkJDM",
    "FkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE8YnXXfaUgmnMtOXU/IncWalRhebrXm",
    "ckC8vdgJ1p5Be5F/3YC8OthxM4+k1M6aEAEFcGzkJiNy6J84y7uzo9M6NyMHAwCQY",
    "DVR0TBAIwADAfBgNVHSMEGDAWgBRm3WjLa38lbEYCuiCPct0ZaSED2DAOBgNVHQ8B",
    "Af8EBAMCBsAwEwYDVR0lBAwwCgYIKwYBBQUHAwIwHQYDVR0RAQH/BBMwEYEPYmRjQ",
    "GV4YW1wbGUuY29tMAoGCCqGSM49BAMCA0gAMEUCIBHda/r1vaL6G3VliL4/Di6YK0",
    "Q6bMjeSkC3dFCOOB8TAiEAx/kHSB4urmiZ0NX5r5XarmPk0wmuydBVoU4hBVZ1yhk=:"
);

fn content_digest() -> HeaderName {
    HeaderName::from_static("content-digest")
}

fn client_cert() -> HeaderName {
    HeaderName::from_static("client-cert")
}

fn header(name: HeaderName) -> MessageSignatureComponent {
    MessageSignatureComponent::header(name)
}

fn test_request_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        DATE,
        HeaderValue::from_static("Tue, 20 Apr 2021 02:07:55 GMT"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        content_digest(),
        HeaderValue::from_static(REQUEST_CONTENT_DIGEST),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("18"));
    headers
}

fn signature_base(
    config: MessageSignatureConfig,
    method: Method,
    target_uri: &str,
    request_target: &str,
    headers: &HeaderMap,
) -> String {
    let target_uri: Uri = target_uri.parse().unwrap();
    let request_target: Uri = request_target.parse().unwrap();
    config
        .signature_base(&method, &target_uri, &request_target, headers)
        .unwrap()
        .into_string()
}

#[test]
fn appendix_b22_query_param_request_base() {
    // RFC 9421 Appendix B.2.2, lines 4704-4715.
    let config = MessageSignatureConfig::new("sig-b22")
        .unwrap()
        .component(MessageSignatureComponent::authority())
        .component(header(content_digest()))
        .component(MessageSignatureComponent::query_param("Pet").unwrap())
        .created(1_618_884_473)
        .key_id("test-key-rsa-pss")
        .tag("header-example");

    let base = signature_base(
        config,
        Method::POST,
        TEST_REQUEST_URI,
        TEST_REQUEST_TARGET,
        &test_request_headers(),
    );

    assert_eq!(
        base,
        format!(
            "\
\"@authority\": example.com\n\
\"content-digest\": {REQUEST_CONTENT_DIGEST}\n\
\"@query-param\";name=\"Pet\": dog\n\
\"@signature-params\": (\"@authority\" \"content-digest\" \"@query-param\";name=\"Pet\");created=1618884473;keyid=\"test-key-rsa-pss\";tag=\"header-example\""
        )
    );
}

#[test]
fn appendix_b23_full_coverage_request_base() {
    // RFC 9421 Appendix B.2.3, lines 4738-4761.
    let config = MessageSignatureConfig::new("sig-b23")
        .unwrap()
        .component(header(DATE))
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::path())
        .component(MessageSignatureComponent::query())
        .component(MessageSignatureComponent::authority())
        .component(header(CONTENT_TYPE))
        .component(header(content_digest()))
        .component(header(CONTENT_LENGTH))
        .created(1_618_884_473)
        .key_id("test-key-rsa-pss");

    let base = signature_base(
        config,
        Method::POST,
        TEST_REQUEST_URI,
        TEST_REQUEST_TARGET,
        &test_request_headers(),
    );

    assert_eq!(
        base,
        format!(
            "\
\"date\": Tue, 20 Apr 2021 02:07:55 GMT\n\
\"@method\": POST\n\
\"@path\": /foo\n\
\"@query\": ?param=Value&Pet=dog\n\
\"@authority\": example.com\n\
\"content-type\": application/json\n\
\"content-digest\": {REQUEST_CONTENT_DIGEST}\n\
\"content-length\": 18\n\
\"@signature-params\": (\"date\" \"@method\" \"@path\" \"@query\" \"@authority\" \"content-type\" \"content-digest\" \"content-length\");created=1618884473;keyid=\"test-key-rsa-pss\""
        )
    );
}

#[test]
fn appendix_b25_hmac_request_base_and_header_formatting() {
    // RFC 9421 Appendix B.2.5, lines 4828-4850.
    let config = MessageSignatureConfig::new("sig-b25")
        .unwrap()
        .component(header(DATE))
        .component(MessageSignatureComponent::authority())
        .component(header(CONTENT_TYPE))
        .created(1_618_884_473)
        .key_id("test-shared-secret");

    let base = signature_base(
        config.clone(),
        Method::POST,
        TEST_REQUEST_URI,
        TEST_REQUEST_TARGET,
        &test_request_headers(),
    );

    assert_eq!(
        base,
        "\
\"date\": Tue, 20 Apr 2021 02:07:55 GMT\n\
\"@authority\": example.com\n\
\"content-type\": application/json\n\
\"@signature-params\": (\"date\" \"@authority\" \"content-type\");created=1618884473;keyid=\"test-shared-secret\""
    );

    let signature = base64::engine::general_purpose::STANDARD
        .decode("pxcQw6G3AjtMBQjwo8XzkZf/bws5LelbaMk5rGIGtE8=")
        .unwrap();
    let headers = config.headers_from_signature(signature).unwrap();

    assert_eq!(
        headers.signature_input.to_str().unwrap(),
        "sig-b25=(\"date\" \"@authority\" \"content-type\");created=1618884473;keyid=\"test-shared-secret\""
    );
    assert_eq!(
        headers.signature.to_str().unwrap(),
        "sig-b25=:pxcQw6G3AjtMBQjwo8XzkZf/bws5LelbaMk5rGIGtE8=:"
    );
}

#[test]
fn appendix_b26_ed25519_request_base() {
    // RFC 9421 Appendix B.2.6, lines 4855-4872.
    let config = MessageSignatureConfig::new("sig-b26")
        .unwrap()
        .component(header(DATE))
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::path())
        .component(MessageSignatureComponent::authority())
        .component(header(CONTENT_TYPE))
        .component(header(CONTENT_LENGTH))
        .created(1_618_884_473)
        .key_id("test-key-ed25519");

    let base = signature_base(
        config,
        Method::POST,
        TEST_REQUEST_URI,
        TEST_REQUEST_TARGET,
        &test_request_headers(),
    );

    assert_eq!(
        base,
        "\
\"date\": Tue, 20 Apr 2021 02:07:55 GMT\n\
\"@method\": POST\n\
\"@path\": /foo\n\
\"@authority\": example.com\n\
\"content-type\": application/json\n\
\"content-length\": 18\n\
\"@signature-params\": (\"date\" \"@method\" \"@path\" \"@authority\" \"content-type\" \"content-length\");created=1618884473;keyid=\"test-key-ed25519\""
    );
}

#[test]
fn appendix_b3_tls_terminating_proxy_request_base() {
    // RFC 9421 Appendix B.3, lines 4885-4956.
    let mut headers = HeaderMap::new();
    headers.insert(client_cert(), HeaderValue::from_static(CLIENT_CERT));
    let config = MessageSignatureConfig::new("ttrp")
        .unwrap()
        .component(MessageSignatureComponent::path())
        .component(MessageSignatureComponent::query())
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::authority())
        .component(header(client_cert()))
        .created(1_618_884_473)
        .key_id("test-key-ecc-p256");

    let base = signature_base(
        config,
        Method::POST,
        "https://service.internal.example/foo?param=Value&Pet=dog",
        TEST_REQUEST_TARGET,
        &headers,
    );

    assert_eq!(
        base,
        format!(
            "\
\"@path\": /foo\n\
\"@query\": ?param=Value&Pet=dog\n\
\"@method\": POST\n\
\"@authority\": service.internal.example\n\
\"client-cert\": {CLIENT_CERT}\n\
\"@signature-params\": (\"@path\" \"@query\" \"@method\" \"@authority\" \"client-cert\");created=1618884473;keyid=\"test-key-ecc-p256\""
        )
    );
}

#[test]
fn appendix_b4_safe_request_transformations_keep_base_stable() {
    // RFC 9421 Appendix B.4, lines 4995-5077.
    let expected = appendix_b4_original_base();

    assert_eq!(
        appendix_b4_base(
            Method::GET,
            "https://example.org/demo?name1=Value1&Name2=value2",
            "/demo?name1=Value1&Name2=value2",
            &appendix_b4_repeated_accept_headers(),
        ),
        expected
    );
    assert_eq!(
        appendix_b4_base(
            Method::GET,
            "https://example.org/demo?name1=Value1&Name2=value2&param=added",
            "/demo?name1=Value1&Name2=value2&param=added",
            &appendix_b4_repeated_accept_headers(),
        ),
        expected
    );
    assert_eq!(
        appendix_b4_base(
            Method::GET,
            "https://example.org/demo?name1=Value1&Name2=value2",
            "/demo?name1=Value1&Name2=value2",
            &appendix_b4_collapsed_accept_headers(),
        ),
        expected
    );
}

#[test]
fn appendix_b4_unsafe_request_transformations_change_base() {
    // RFC 9421 Appendix B.4, lines 5079-5112.
    let original = appendix_b4_original_base();
    let changed_authority = appendix_b4_base(
        Method::POST,
        "https://example.com/demo?name1=Value1&Name2=value2",
        "/demo?name1=Value1&Name2=value2",
        &appendix_b4_repeated_accept_headers(),
    );
    let reordered_accept = appendix_b4_base(
        Method::GET,
        "https://example.org/demo?name1=Value1&Name2=value2",
        "/demo?name1=Value1&Name2=value2",
        &appendix_b4_reordered_accept_headers(),
    );

    assert_ne!(changed_authority, original);
    assert!(changed_authority.contains("\"@method\": POST"));
    assert!(changed_authority.contains("\"@authority\": example.com"));

    assert_ne!(reordered_accept, original);
    assert!(reordered_accept.contains("\"accept\": */*, application/json"));
}

fn appendix_b4_base(
    method: Method,
    target_uri: &str,
    request_target: &str,
    headers: &HeaderMap,
) -> String {
    let config = MessageSignatureConfig::new("transform")
        .unwrap()
        .component(MessageSignatureComponent::method())
        .component(MessageSignatureComponent::path())
        .component(MessageSignatureComponent::authority())
        .component(header(ACCEPT))
        .created(1_618_884_473)
        .key_id("test-key-ed25519");
    signature_base(config, method, target_uri, request_target, headers)
}

fn appendix_b4_repeated_accept_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append(ACCEPT, HeaderValue::from_static("application/json"));
    headers.append(ACCEPT, HeaderValue::from_static("*/*"));
    headers
}

fn appendix_b4_collapsed_accept_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json, */*"));
    headers
}

fn appendix_b4_reordered_accept_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append(ACCEPT, HeaderValue::from_static("*/*"));
    headers.append(ACCEPT, HeaderValue::from_static("application/json"));
    headers
}

fn appendix_b4_original_base() -> String {
    "\
\"@method\": GET\n\
\"@path\": /demo\n\
\"@authority\": example.org\n\
\"accept\": application/json, */*\n\
\"@signature-params\": (\"@method\" \"@path\" \"@authority\" \"accept\");created=1618884473;keyid=\"test-key-ed25519\""
        .to_owned()
}
