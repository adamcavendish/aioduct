# HTTP Message Signatures

aioduct provides RFC 9421 HTTP Message Signatures helpers for request and
response signature bases, parsed verification, `Accept-Signature` negotiation,
covered `Content-Digest` verification, caller-supplied trailer field coverage
with `;tr`, native automatic request signing, native buffered request
`Content-Digest` generation, bounded forward response `Content-Digest`
generation, and forward-only automatic response signing. The portable helpers
build signature bases, format and parse `Signature-Input` / `Signature` header
values, turn accepted signature requests into concrete signing configs, apply
verification policy checks, and expose the bytes callers pass to cryptographic
code. Callers still choose the cryptographic signing and verification algorithms.
Native clients can also insert SHA-256 `Content-Digest` for buffered request
bodies, generate bounded downstream response digests for forwards, and run
synchronous or asynchronous signers automatically for each finalized request
attempt or for a forwarded downstream response.

## Core Flow

```rust,no_run
use aioduct::{MessageSignatureComponent, MessageSignatureConfig};
use http::{HeaderMap, HeaderValue, Method, Uri};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let mut headers = HeaderMap::new();
headers.insert(http::header::DATE, HeaderValue::from_static("Tue, 20 Apr 2021 02:07:55 GMT"));

let target_uri: Uri = "https://example.com/foo?param=Value".parse()?;
let request_target: Uri = "/foo?param=Value".parse()?;

let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::method())
    .component(MessageSignatureComponent::authority())
    .component(MessageSignatureComponent::path())
    .component(MessageSignatureComponent::header(http::header::DATE))
    .created(1_618_884_473)
    .key_id("test-key");

let base = config.signature_base(&Method::GET, &target_uri, &request_target, &headers)?;
let signature_bytes = my_signing_function(base.as_bytes());
let signature_headers = config.headers_from_signature(signature_bytes)?;
signature_headers.insert_into(&mut headers)?;
# Ok(())
# }
# fn my_signing_function(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

`target_uri` is the full URI for derived components such as `@scheme`,
`@authority`, `@target-uri`, `@path`, and `@query`. `request_target` is the
actual request URI form that will be sent on the wire and is used for
`@request-target`. This distinction matters for forwarding and CONNECT-style
requests.

Use `MessageSignatureRequestContext::with_trailers(...)` or
`MessageSignatureResponseContext::with_trailers(...)` with the `*_for_context()`
helpers when a signature covers a trailer field with `;tr`. aioduct reads those
values only from the attached trailer map; header fields with the same name are
signed separately, matching RFC 9421. Trailer components can also use `;sf`,
`;key`, `;bs`, and response related-request `;req` where those parameters are
otherwise valid.

## Supported Components

| Component | Source |
| --- | --- |
| `@method` | Request method, preserving case. |
| `@scheme` | Lowercase target URI scheme. |
| `@authority` | Target URI authority with lowercase host and default `http:80` / `https:443` ports omitted. |
| `@request-target` | The actual final request URI form. |
| `@target-uri` | The full target URI. |
| `@path` | Target URI path, with an empty path normalized to `/`. |
| `@query` | Target URI query with a leading `?`; absent query signs as `?`. |
| `@status` | Response status code with no reason phrase. |
| Header and trailer fields | Lowercase field names; repeated values are joined with `, `. Supports `;sf`, `;key`, `;bs`, and caller-supplied `;tr` component parameters. |

When building a response signature base with a related request,
`MessageSignatureComponent::related_request()` adds the `;req` parameter and
derives that component from the triggering request.

Missing covered headers, duplicate component identifiers, invalid labels,
non-ASCII generated signature bases, and covered header values that cannot be
represented as ASCII header fields return `MessageSignatureError`.

## Native Automatic Signing

Native tokio, smol, and compio clients can sign requests automatically with
`HttpEngineBuilder::message_signature(config, signer)` for synchronous signers,
`message_signature_async(config, signer)` for send-runtime async signers, or
`message_signature_async_local(config, signer)` for local-runtime async signing
futures. The signer runs after default headers, cookies, cache validators,
middleware, digest-auth retry headers, forwarding request rewrites, and request
framing cleanup have finalized each native dispatch attempt. Stale
pooled-connection replays are re-signed before retrying.

```rust,no_run
use aioduct::{HttpEngineSend, MessageSignatureComponent, MessageSignatureConfig};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::method())
    .component(MessageSignatureComponent::authority())
    .component(MessageSignatureComponent::path())
    .key_id("test-key");

let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
    .message_signature(config, |base: &[u8]| {
        Ok(sign_with_your_key(base))
    })
    .build()?;
# let _ = client;
# Ok(())
# }
# fn sign_with_your_key(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

Async automatic signers receive an owned `MessageSignatureBase`, so request and
header borrows do not cross the signer await boundary:

```rust,no_run
use aioduct::{HttpEngineSend, MessageSignatureBase, MessageSignatureComponent, MessageSignatureConfig};
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = MessageSignatureConfig::new("sig1")?
    .component(MessageSignatureComponent::method())
    .component(MessageSignatureComponent::authority())
    .key_id("test-key");

let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
    .message_signature_async(config, |base: MessageSignatureBase| async move {
        Ok(sign_with_remote_key(base.as_bytes()).await)
    })
    .build()?;
# let _ = client;
# Ok(())
# }
# async fn sign_with_remote_key(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

When automatic signing is configured, aioduct owns its configured signature
label in the `Signature-Input` and `Signature` request fields. It preserves
unrelated labels and replaces the configured label on every signed attempt. If
the signer fails, the request is not dispatched.

Forwarded requests are signed after hop-by-hop cleanup, upstream URI rewriting,
explicit header forwarding/removal, and `on_request` hooks. Components derived
from the target URI use the upstream URI; `@request-target` uses the final URI
form sent on the wire.

Forward builders can also sign the response returned downstream with
`response_message_signature(...)`, `response_message_signature_async(...)`, or
`response_message_signature_async_local(...)`. Response signing runs after
upstream response hop-by-hop headers are stripped and after `on_response` runs,
then strips hop-by-hop headers again before generating the base. Related-request
components use the inbound request snapshot, not the rewritten upstream request.
For origin-form inbound requests, set `downstream_target_uri(...)` when the
response signature covers related-request `@scheme`, `@authority`, or
`@target-uri`. Automatic response signing rejects `CONNECT`, known upgrade
requests, HTTP/1.1 `101 Switching Protocols` responses, and trailer components.
Use `response_content_digest(max_bytes)` to buffer a forwarded response up to a
fixed cap and insert `Content-Digest` before response signing, allowing the
signature to cover `content-digest` without unbounded buffering.

## Automatic Content-Digest

Native clients can opt in to SHA-256 `Content-Digest` generation with
`HttpEngineBuilder::automatic_content_digest(true)` or override it per request
with `RequestBuilderSend::automatic_content_digest(...)` /
`RequestBuilderLocal::automatic_content_digest(...)`. When enabled, aioduct
inserts `Content-Digest: sha-256=:...:` for buffered request bodies that do not
already have a `Content-Digest` header. Requests without a configured body are
left unchanged; use an explicitly empty buffered body to sign an empty-body
digest.

Digest insertion happens after middleware and framing-header cleanup and before
automatic message signing. A signature that covers `content-digest` therefore
covers the generated value. If a request already has `Content-Digest`, aioduct
preserves it and signs that caller-supplied value.

aioduct does not auto-buffer streaming bodies and does not generate digest or
signature trailers. Streaming bodies and middleware-replaced bodies must provide
an explicit `Content-Digest` header when automatic digest generation is enabled.
Use `sha256_content_digest_value(...)` when the complete body is already in
memory, or `sha256_content_digest_value_from_digest(...)` when a streaming caller
has precomputed the 32-byte SHA-256 digest out-of-band.

```rust,no_run
use aioduct::{CONTENT_DIGEST, sha256_content_digest_value_from_digest};
use http::{HeaderMap, HeaderName};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let mut headers = HeaderMap::new();
let digest = precomputed_stream_digest();
headers.insert(
    HeaderName::from_static(CONTENT_DIGEST),
    sha256_content_digest_value_from_digest(digest)?,
);
# Ok(())
# }
# fn precomputed_stream_digest() -> [u8; 32] { [0_u8; 32] }
```

## Manual And Async Signers

For async, host-backed, WebCrypto, KMS, or HSM signing, build the signature base,
await the external signer yourself, then call `headers_from_signature()` with the
returned bytes. This avoids blocking a runtime thread.

The synchronous `MessageSignatureSigner` trait is used by native automatic
signing and local CPU-bound signing. Do not use a blocking network or device
call inside that synchronous signer on an async runtime thread.

## Response Signature Bases

Use `response_signature_base()` for response-only signatures and
`request_response_signature_base()` when the response signature covers parts of
the related request with `;req`. Use the `*_for_context()` variants when the
covered components include caller-supplied trailer fields with `;tr`.

```rust,no_run
use aioduct::{MessageSignatureComponent, MessageSignatureConfig};
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let target_uri: Uri = "https://example.com/foo?param=Value".parse()?;
let request_target: Uri = "/foo?param=Value".parse()?;
let mut request_headers = HeaderMap::new();
request_headers.insert(http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

let mut response_headers = HeaderMap::new();
response_headers.insert(http::header::CONTENT_TYPE, HeaderValue::from_static("application/problem+json"));

let config = MessageSignatureConfig::new("reqres")?
    .component(MessageSignatureComponent::status())
    .component(MessageSignatureComponent::header(http::header::CONTENT_TYPE))
    .component(MessageSignatureComponent::method().related_request())
    .component(MessageSignatureComponent::path().related_request())
    .created(1_618_884_479)
    .key_id("test-key");

let base = config.request_response_signature_base(
    &Method::POST,
    &target_uri,
    &request_target,
    &request_headers,
    StatusCode::SERVICE_UNAVAILABLE,
    &response_headers,
)?;
let signature_headers = config.headers_from_signature(my_signing_function(base.as_bytes()))?;
# let _ = signature_headers;
# Ok(())
# }
# fn my_signing_function(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

`sign_response()` provides the same synchronous signer callback pattern as
`sign_request()` for response-only bases. For response bases that also cover a
related request, sign `request_response_signature_base()` output and pass the
signature bytes to `headers_from_signature()`. Native automatic response signing
is available on forward builders only. Forward builders can also generate a
bounded response `Content-Digest` before signing;
`HttpEngineBuilder::message_signature()` continues to configure request signing.

## Accept-Signature

Use `AcceptSignature` to parse or build RFC 9421 `Accept-Signature` dictionaries.
Each `AcceptSignatureEntry` names the requested output signature label, the
covered components, and requested metadata such as `created`, `expires`, `alg`,
`keyid`, `nonce`, and `tag`.

```rust,no_run
use aioduct::{AcceptSignature, AcceptSignatureEntry, MessageSignatureComponent};
use http::HeaderMap;

# fn example(mut headers: HeaderMap) -> Result<(), Box<dyn std::error::Error>> {
let accept = AcceptSignature::new().entry(
    AcceptSignatureEntry::new("sig1")?
        .component(MessageSignatureComponent::status())
        .component(MessageSignatureComponent::method().related_request())
        .created()
        .key_id("test-key"),
);

accept.validate_request_response_target()?;
accept.insert_into(&mut headers)?;
# Ok(())
# }
```

Use `validate_request_target()` when an `Accept-Signature` response asks the
client to sign its next request. Use `validate_request_response_target()` when an
`Accept-Signature` request asks the server to sign the response and that response
signature can cover related request components with `;req`.

`AcceptSignatureFulfillment` provides concrete metadata values, such as generated
`created` and `expires` timestamps. The `*_signature_config()` helpers validate
target-message applicability, copy requested components and metadata into a
`MessageSignatureConfig`, and fail closed when a requested timestamp is missing
or a supplied metadata value conflicts with the request.

```rust,no_run
use aioduct::{AcceptSignature, AcceptSignatureFulfillment};
use http::{HeaderMap, Method, StatusCode, Uri};

# fn example(request_headers: HeaderMap, mut response_headers: HeaderMap) -> Result<(), Box<dyn std::error::Error>> {
let accept = AcceptSignature::from_headers(&request_headers)?;
let fulfillment = AcceptSignatureFulfillment::new()
    .created(1_618_884_500)
    .key_id("test-key");

let target_uri: Uri = "https://example.com/foo".parse()?;
let request_target: Uri = "/foo".parse()?;

for config in accept.request_response_signature_configs(&fulfillment)? {
    let base = config.request_response_signature_base(
        &Method::GET,
        &target_uri,
        &request_target,
        &request_headers,
        StatusCode::OK,
        &response_headers,
    )?;
    let signature = my_signing_function(base.as_bytes());
    config.headers_from_signature(signature)?.insert_into(&mut response_headers)?;
}
# Ok(())
# }
# fn my_signing_function(_: &[u8]) -> Vec<u8> { vec![1, 2, 3] }
```

Fulfillment remains explicit: callers still choose which requests to honor,
select signing keys, generate timestamps, run cryptography, and attach the
resulting `Signature-Input` / `Signature` fields. Receivers can ignore an
unacceptable request by selecting individual `AcceptSignatureEntry` values
instead of fulfilling the whole dictionary.

## Request Verification

`MessageSignature::from_headers(&headers, "sig1")` parses existing
`Signature-Input` and `Signature` fields, selects one label, exposes known
metadata parameters such as `created`, `expires`, `alg`, and `keyid`, and returns
the decoded signature bytes. It rejects malformed dictionaries, duplicate
labels, mismatched labels, and unknown selected labels.

The parsed value can rebuild the request signature base for fully manual
caller-owned crypto verification:

```rust,no_run
use aioduct::MessageSignature;
use http::{HeaderMap, Method, Uri};

# fn example(headers: HeaderMap) -> Result<(), Box<dyn std::error::Error>> {
let target_uri: Uri = "https://example.com/foo?param=Value".parse()?;
let request_target: Uri = "/foo?param=Value".parse()?;
let parsed = MessageSignature::from_headers(&headers, "sig1")?;
let base = parsed.signature_base(&Method::GET, &target_uri, &request_target, &headers)?;

verify_with_your_key(base.as_bytes(), parsed.signature(), parsed.params());
# Ok(())
# }
# fn verify_with_your_key(_: &[u8], _: &[u8], _: &aioduct::MessageSignatureParams) {}
```

For common verification policy checks, use
`MessageSignatureVerificationPolicy`. The policy parses a selected label,
requires covered components, filters accepted `alg` and `keyid` metadata, checks
`created` / `expires` timestamps with optional clock skew and maximum age, then
calls your verifier with the selected label, parsed params, rebuilt base bytes,
and decoded signature bytes.

When body bytes are available, attach them to the request or response context
with `with_body(...)`. If the selected signature covers `content-digest`, the
policy verifies a SHA-256 `Content-Digest` field before rebuilding the signature
base and before invoking your verifier. For response signatures that cover a
related request field with `;req`, attach the related request body to
`MessageSignatureRequestContext`. If no body bytes are attached, verification
preserves the previous signature-only behavior and does not check the digest
field. Malformed digest fields, digest fields without `sha-256`, and mismatched
body bytes fail closed with `MessageSignatureError` before your verifier runs.
Attach trailer maps with `with_trailers(...)` when the selected signature covers
trailer fields with `;tr`; without an attached trailer map, those covered
components fail closed before the verifier runs.

When the selected signature carries `created` or `expires`, configure
`validation_time()` so the policy can validate those timestamps. Without a
validation time, verification fails closed with `MissingValidationTime`.

```rust,no_run
use aioduct::{
    MessageSignatureComponent, MessageSignatureVerificationInput,
    MessageSignatureVerificationPolicy,
};
use http::{HeaderMap, Method, Uri};

# fn example(headers: HeaderMap) -> Result<(), Box<dyn std::error::Error>> {
let target_uri: Uri = "https://example.com/foo?param=Value".parse()?;
let request_target: Uri = "/foo?param=Value".parse()?;

let policy = MessageSignatureVerificationPolicy::new()
    .required_component(MessageSignatureComponent::method())
    .required_component(MessageSignatureComponent::authority())
    .accepted_algorithm("ed25519")
    .accepted_key_id("test-key")
    .validation_time(1_618_884_500)
    .max_age(300)
    .clock_skew(5);

policy.verify_request(
    &headers,
    "sig1",
    &Method::GET,
    &target_uri,
    &request_target,
    &|input: MessageSignatureVerificationInput<'_>| {
        Ok(verify_with_your_key(
            input.params(),
            input.signature_base(),
            input.signature(),
        ))
    },
)?;
# Ok(())
# }
# fn verify_with_your_key(
#     _: &aioduct::MessageSignatureParams,
#     _: &[u8],
#     _: &[u8],
# ) -> bool {
#     true
# }
```

For request body integrity, use the request context form:

```rust,no_run
use aioduct::{
    MessageSignatureRequestContext, MessageSignatureVerificationInput,
    MessageSignatureVerificationPolicy,
};
use http::{HeaderMap, Method, Uri};

# fn example(headers: HeaderMap, body: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
let target_uri: Uri = "https://example.com/foo".parse()?;
let request_target: Uri = "/foo".parse()?;
let request = MessageSignatureRequestContext::new(
    &Method::POST,
    &target_uri,
    &request_target,
    &headers,
)
.with_body(body);

MessageSignatureVerificationPolicy::new().verify_request_context(
    request,
    "sig1",
    &|input: MessageSignatureVerificationInput<'_>| {
        Ok(verify_with_your_key(input.signature_base(), input.signature()))
    },
)?;
# Ok(())
# }
# fn verify_with_your_key(_: &[u8], _: &[u8]) -> bool { true }
```

`MessageSignature::verify_request()` applies the same policy to an already parsed
request signature. `verify_request_context()` is the parsed-signature equivalent
for body-aware request verification. `verify_response()` verifies response-only
signatures, and `verify_request_response()` verifies response signatures that
bind selected components from the originating request with `;req`.

```rust,no_run
use aioduct::{
    MessageSignatureComponent, MessageSignatureRequestContext,
    MessageSignatureResponseContext, MessageSignatureVerificationInput,
    MessageSignatureVerificationPolicy,
};
use http::{HeaderMap, Method, StatusCode, Uri};

# fn example(request_headers: HeaderMap, response_headers: HeaderMap) -> Result<(), Box<dyn std::error::Error>> {
let target_uri: Uri = "https://example.com/foo?param=Value".parse()?;
let request_target: Uri = "/foo?param=Value".parse()?;
let request = MessageSignatureRequestContext::new(
    &Method::POST,
    &target_uri,
    &request_target,
    &request_headers,
);
let response = MessageSignatureResponseContext::new(StatusCode::OK, &response_headers);

let policy = MessageSignatureVerificationPolicy::new()
    .required_component(MessageSignatureComponent::status())
    .required_component(MessageSignatureComponent::method().related_request())
    .accepted_key_id("test-key")
    .validation_time(1_618_884_500);

policy.verify_request_response(
    request,
    response,
    "sig1",
    &|input: MessageSignatureVerificationInput<'_>| {
        Ok(verify_with_your_key(
            input.params(),
            input.signature_base(),
            input.signature(),
        ))
    },
)?;
# Ok(())
# }
# fn verify_with_your_key(
#     _: &aioduct::MessageSignatureParams,
#     _: &[u8],
#     _: &[u8],
# ) -> bool {
#     true
# }
```

## Header Ownership

When automatic signing is not configured, user-supplied `Signature` and
`Signature-Input` headers are ordinary headers and are preserved. Native
automatic request and forward-response signing own their configured label in
those two fields when configured: they replace that label on each signed message
while preserving unrelated labels, so redirects, retries, digest-auth retries,
forwarding rewrites, stale connection replays, and forwarded response mutations
cannot send signatures for an earlier message shape.

## Runtime Coverage

The helpers are portable and can be used with native, blocking, wasm, and wasi-p2
request builders by inserting the generated headers manually. Caller-supplied
trailer maps for `;tr` components are portable context inputs, not automatic
trailer generation. Native automatic request signing supports synchronous and
asynchronous signers for tokio, smol, and compio request dispatch. Forward-only
automatic response signing supports synchronous signers, send-runtime async
signers for tokio/smol, and local async signers for compio. Buffered automatic
`Content-Digest` generation is available for native request bodies, and bounded
forward response `Content-Digest` generation is available on native forward
builders. Blocking clients inherit configured native-client behavior.

Browser Fetch and WASI hosts can still alter or reject some headers at the host
boundary. That host behavior is outside aioduct's control.

## RFC 9421 Conformance Matrix

This matrix tracks RFC 9421 example coverage against the current public API.
Rows marked supported are covered by normal Rust tests, not ignored or
expected-failing tests. Rows marked planned remain visible here until their owner
work lands.

| RFC 9421 area | Status | Test coverage | Owner | Notes |
| --- | --- | --- | --- | --- |
| Appendix B.2.3 full request coverage | Supported | `appendix_b23_full_coverage_request_base` | Current | Covers request derived components, plain fields, `Content-Digest` as a caller-supplied header, and signature parameters. |
| Appendix B.2.5 HMAC request example | Supported | `appendix_b25_hmac_request_base_and_header_formatting` | Current | Tests request base and single-label header formatting with caller-supplied signature bytes. It does not test HMAC itself. |
| Appendix B.2.6 Ed25519 request example | Supported | `appendix_b26_ed25519_request_base` | Current | Tests request base only; Ed25519 signing remains caller-owned. |
| Appendix B.3 TLS-terminating proxy request base | Supported | `appendix_b3_tls_terminating_proxy_request_base` | Current | Covers proxy-style authority and a long `Client-Cert` field value. |
| Appendix B.4 request transformations | Supported | `appendix_b4_safe_request_transformations_keep_base_stable`, `appendix_b4_unsafe_request_transformations_change_base` | Current | Covers stable base strings across safe transformations and changed base strings for covered method/authority or reordered same-name fields. |
| Appendix B.2.1 empty covered component set | Supported | `empty_covered_component_set_builds_signature_params_only_base`, `parsed_signature_accepts_empty_covered_set`, `verification_policy_allows_empty_covered_component_set` | Current | Supports `Signature-Input` values like `sig1=();...` and builds a signature base containing only `@signature-params`. The RFC discourages empty sets; verifiers can require concrete components with policy. |
| Appendix B.2.2 `@query-param` | Supported | `appendix_b22_query_param_request_base` | Current | Covers named query parameter parsing, form-style decoding, percent-encoded component identifiers, and missing/duplicate parameter errors. |
| Component parameter `;bs` | Supported | `byte_sequence_header_values_are_signed_as_structured_field_list` | Current | Covers Byte Sequence wrapping for caller-supplied header field values. |
| Component parameter `;key` | Supported | `dictionary_key_header_values_are_signed_as_structured_field_members`, `dictionary_key_missing_malformed_and_duplicate_values` | Current | Covers Dictionary Structured Field member selection, strict member serialization, missing key errors, malformed dictionary errors, and duplicate source keys using the RFC 9651 last-value rule. |
| Component parameter `;sf` | Supported | `structured_field_header_values_are_signed_with_strict_serialization` | Current | Covers strict serialization for valid RFC 9651 Dictionary, List, and Item field values. |
| Component parameter `;tr` | Supported | `response_context_uses_caller_supplied_trailer_fields`, `trailer_fields_are_distinct_from_headers_and_support_field_parameters`, `trailer_components_require_attached_trailer_fields`, `verification_policy_calls_verifier_with_trailer_components` | Current | Covers caller-supplied request and response trailer fields, keeps same-name header and trailer fields separate, composes with `;sf`, `;key`, `;bs`, and related request `;req`. Automatic trailer generation remains future work. |
| Response `@status` and response signature bases | Supported | `builds_response_signature_base_for_status_and_headers`, `section_24_response_with_related_request_base`, `sign_response_uses_signer_callback`, `forward_response_signature_covers_response_hook_and_strips_hop_by_hop`, `test_compio_forward_response_message_signature` | Current | Builds response signature bases, formats response signature headers from caller-supplied signature bytes, and can automatically sign forwarded downstream responses on native send/local runtimes. |
| Related request components `;req` | Supported | `request_response_signature_base_uses_related_request_components`, `parsed_signature_rebuilds_response_and_related_request_base`, `response_signature_rejects_components_from_wrong_context` | Current | Routes `;req` components to the related request when building response bases and rejects `;req` on request targets or without related request context. |
| Multiple signature dictionaries | Supported | `insert_into_merges_signature_headers_by_label`, `automatic_signing_merges_existing_signature_headers_by_label` | Current | Generated signatures parse existing `Signature-Input` and `Signature` dictionaries, reject duplicate or mismatched labels, preserve unrelated labels, and replace only the configured label. |
| Parsed signature selection and request-base rebuild | Supported | `parsed_signature_selects_label_and_rebuilds_request_base`, `parsed_signature_handles_component_parameters`, `parsed_signature_reports_selection_and_header_errors` | Current | Parses selected `Signature-Input` / `Signature` labels, exposes known metadata and signature bytes, preserves extension metadata in the rebuilt base, and rejects malformed or mismatched fields. |
| Message verification policy API | Supported | `verification_policy_calls_verifier_with_rebuilt_base`, `verification_policy_calls_verifier_with_response_base`, `verification_policy_calls_verifier_with_related_request_response_base`, `parsed_response_signature_can_verify_with_policy`, `verification_policy_reports_selection_and_header_errors`, `verification_policy_rejects_unacceptable_signature_metadata`, `verification_policy_rejects_failed_verifier_callback` | Current | Applies required-component, accepted-algorithm, accepted-key-id, timestamp, max-age, and verifier-callback checks for selected request, response, and request-response signatures. Cryptographic verification remains caller-owned. |
| Covered `Content-Digest` verification | Supported | `verification_policy_checks_request_content_digest_before_signature`, `verification_policy_rejects_mismatched_request_content_digest_before_signature`, `verification_policy_rejects_malformed_and_unsupported_content_digest_before_signature`, `verification_policy_checks_response_content_digest_before_signature`, `verification_policy_checks_related_request_content_digest_before_signature`, `verification_policy_skips_content_digest_check_when_body_is_unavailable` | Current | Verifies SHA-256 `Content-Digest` before caller-owned signature verification when body bytes are attached and the selected signature covers the whole `content-digest` field or its `sha-256` dictionary member, including related request fields with `;req`. |
| `Accept-Signature` parser and builder | Supported | `accept_signature_parses_rfc_style_request`, `accept_signature_formats_and_inserts_header`, `accept_signature_from_headers_combines_field_values`, `accept_signature_reports_header_errors`, `accept_signature_validates_target_message_components` | Current | Parses and formats requested signature dictionaries, exposes requested metadata, and validates request, response, or request-response target component applicability. |
| `Accept-Signature` fulfillment helpers | Supported | `accept_signature_fulfills_response_with_related_request`, `accept_signature_fulfills_next_request`, `accept_signature_fulfillment_reports_unfulfillable_requests`, `accept_signature_allows_ignoring_requests_and_adding_signatures` | Current | Converts accepted entries into concrete `MessageSignatureConfig` values, fills requested metadata, rejects missing or conflicting requested parameters, supports caller-selected ignored requests, and allows additional signatures. Cryptography and header attachment remain caller-owned. |
| SHA-256 `Content-Digest` value helpers | Supported | `formats_sha256_content_digest`, `formats_precomputed_sha256_content_digest`, `inserts_sha256_content_digest` | Current | Builds explicit `Content-Digest` field values from complete body bytes or a precomputed 32-byte SHA-256 digest. |
| Buffered automatic `Content-Digest` generation | Supported | `automatic_content_digest_is_inserted_before_signing`, `automatic_content_digest_preserves_manual_header`, `automatic_content_digest_rejects_streaming_body_without_manual_digest`, `automatic_content_digest_rejects_middleware_replaced_body_without_manual_digest` | Current | Native dispatch can insert SHA-256 `Content-Digest` for buffered bodies before automatic signing. Existing digest fields are preserved; streaming or middleware-replaced bodies need explicit digest fields. |
| Bounded forward response `Content-Digest` generation | Supported | `forward_response_content_digest_is_signed_and_preserves_body`, `forward_response_content_digest_rejects_body_over_limit`, `forward_response_content_digest_rejects_connect_before_upstream`, `forward_response_content_digest_preserves_existing_field`, `test_compio_forward_response_content_digest_is_signed` | Current | Native forward builders can buffer downstream response bodies up to a caller cap, insert SHA-256 `Content-Digest` before response signing, preserve existing digest fields, and fail closed over the cap. |
| Async automatic signing | Supported | `async_automatic_signing_adds_headers_after_middleware`, `async_signer_error_aborts_request_before_dispatch`, `test_compio_async_local_message_signature` | Current | Send-runtime signing uses `message_signature_async` with a `Send` future; local-runtime signing uses `message_signature_async_local` and can await a non-`Send` future. Sync automatic signing remains supported. |
| Automatic trailer-based digest/signature generation | Future follow-up | Matrix only | Post first pass | Trailer fields are standards-valid, but automatic trailer generation needs cross-runtime request-trailer semantics first. |
| Cryptographic algorithm validation | Not in scope | Matrix only | Caller-owned | aioduct builds bases and header values; callers own keys, algorithms, signing, and verification cryptography. |

## Future Work

- Automatic trailer-based digest/signature generation after trailer semantics are proven across runtimes.
