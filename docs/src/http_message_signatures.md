# HTTP Message Signatures

aioduct provides RFC 9421 HTTP Message Signatures request-signing helpers,
parsed request verification helpers, and native automatic request signing. The
portable helpers build signature bases, format and parse `Signature-Input` /
`Signature` header values, apply request verification policy checks, and expose
the bytes callers pass to cryptographic code. Callers still choose the
cryptographic signing and verification algorithms. Native clients can also run a
synchronous signer automatically for each finalized request attempt.

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
| Header fields | Lowercase field names; repeated values are joined with `, `. Supports `;sf`, `;key`, and `;bs` component parameters. |

Missing covered headers, duplicate component identifiers, invalid labels,
non-ASCII generated signature bases, and covered header values that cannot be
represented as ASCII header fields return `MessageSignatureError`.

## Native Automatic Signing

Native tokio, smol, and compio clients can sign requests automatically with
`HttpEngineBuilder::message_signature(config, signer)`. The signer runs after
default headers, cookies, cache validators, middleware, digest-auth retry
headers, forwarding request rewrites, and request framing cleanup have finalized
each native dispatch attempt. Stale pooled-connection replays are re-signed
before retrying.

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

When automatic signing is configured, aioduct owns its configured signature
label in the `Signature-Input` and `Signature` request fields. It preserves
unrelated labels and replaces the configured label on every signed attempt. If
the signer fails, the request is not dispatched.

Forwarded requests are signed after hop-by-hop cleanup, upstream URI rewriting,
explicit header forwarding/removal, and `on_request` hooks. Components derived
from the target URI use the upstream URI; `@request-target` uses the final URI
form sent on the wire.

## Manual And Async Signers

For async, host-backed, WebCrypto, KMS, or HSM signing, build the signature base,
await the external signer yourself, then call `headers_from_signature()` with the
returned bytes. This avoids blocking a runtime thread.

The synchronous `MessageSignatureSigner` trait is used by native automatic
signing and local CPU-bound signing. Do not use a blocking network or device
call inside that synchronous signer on an async runtime thread.

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

For common request verification policy checks, use
`MessageSignatureVerificationPolicy`. The policy parses a selected label,
requires covered components, filters accepted `alg` and `keyid` metadata, checks
`created` / `expires` timestamps with optional clock skew and maximum age, then
calls your verifier with the selected label, parsed params, rebuilt base bytes,
and decoded signature bytes.

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

`MessageSignature::verify_request()` applies the same policy to an already parsed
signature. Response `@status` and related-request `;req` verification remain
future response-support work.

## Header Ownership

When automatic signing is not configured, user-supplied `Signature` and
`Signature-Input` headers are ordinary request headers and are preserved. Native
automatic signing owns its configured label in those two fields when configured:
it replaces that label on each final dispatch so redirects, retries,
digest-auth retries, forwarding rewrites, and stale connection replays cannot
send signatures for an earlier request shape.

## Runtime Coverage

The helpers are portable and can be used with native, blocking, wasm, and wasi-p2
request builders by inserting the generated headers manually. Native automatic
request signing is available for tokio, smol, and compio request dispatch.
Blocking clients inherit it when they wrap a configured native client.

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
| Appendix B.2.1 empty covered component set | Planned | Matrix only | Core model | Current API rejects empty covered component lists. The RFC discourages empty sets, but full parsing/model support should make the policy explicit. |
| Appendix B.2.2 `@query-param` | Supported | `appendix_b22_query_param_request_base` | Current | Covers named query parameter parsing, form-style decoding, percent-encoded component identifiers, and missing/duplicate parameter errors. |
| Component parameter `;bs` | Supported | `byte_sequence_header_values_are_signed_as_structured_field_list` | Current | Covers Byte Sequence wrapping for caller-supplied header field values. |
| Component parameter `;key` | Supported | `dictionary_key_header_values_are_signed_as_structured_field_members`, `dictionary_key_missing_malformed_and_duplicate_values` | Current | Covers Dictionary Structured Field member selection, strict member serialization, missing key errors, malformed dictionary errors, and duplicate source keys using the RFC 9651 last-value rule. |
| Component parameter `;sf` | Supported | `structured_field_header_values_are_signed_with_strict_serialization` | Current | Covers strict serialization for valid RFC 9651 Dictionary, List, and Item field values. |
| Component parameter `;tr` | Planned | Matrix only | Component parameters | `;tr` covers caller-supplied trailer fields only in the first pass. |
| Response `@status` and response signatures | Planned | Matrix only | Response support | Requires response signature contexts and response verification support. |
| Related request components `;req` | Planned | Matrix only | Response support | Requires request-response pair contexts. |
| Multiple signature dictionaries | Supported | `insert_into_merges_signature_headers_by_label`, `automatic_signing_merges_existing_signature_headers_by_label` | Current | Generated signatures parse existing `Signature-Input` and `Signature` dictionaries, reject duplicate or mismatched labels, preserve unrelated labels, and replace only the configured label. |
| Parsed signature selection and request-base rebuild | Supported | `parsed_signature_selects_label_and_rebuilds_request_base`, `parsed_signature_handles_component_parameters`, `parsed_signature_reports_selection_and_header_errors` | Current | Parses selected `Signature-Input` / `Signature` labels, exposes known metadata and signature bytes, preserves extension metadata in the rebuilt base, and rejects malformed or mismatched fields. |
| Request verification policy API | Supported | `verification_policy_calls_verifier_with_rebuilt_base`, `parsed_signature_can_verify_with_policy`, `verification_policy_reports_selection_and_header_errors`, `verification_policy_rejects_unacceptable_signature_metadata`, `verification_policy_rejects_failed_verifier_callback` | Current | Applies required-component, accepted-algorithm, accepted-key-id, timestamp, max-age, and verifier-callback checks for selected request signatures. Cryptographic verification remains caller-owned. |
| `Accept-Signature` negotiation | Planned | Matrix only | Negotiation | Requires parser, builder, and fulfillment logic for requested signatures. |
| Buffered automatic `Content-Digest` generation | Planned | Matrix only | Body digest | Current support signs caller-supplied `Content-Digest` fields. |
| Async automatic signing | Planned | Matrix only | Async signing | Sync automatic signing remains supported; async signing will use explicit send/local APIs. |
| Automatic trailer-based digest/signature generation | Future follow-up | Matrix only | Post first pass | Trailer fields are standards-valid, but automatic trailer generation needs cross-runtime request-trailer semantics first. |
| Cryptographic algorithm validation | Not in scope | Matrix only | Caller-owned | aioduct builds bases and header values; callers own keys, algorithms, signing, and verification cryptography. |

## Future Work

- Async automatic signer callbacks.
- Response signature verification, including `@status` and `;req` support.
- `Accept-Signature` negotiation.
- Component parameter `;tr`.
- Trailer coverage and response `;req` / `@status` components.
- Automatic `Content-Digest` generation for buffered request bodies.
- Precomputed digest helpers for streaming bodies.
- Automatic trailer-based digest/signature generation after trailer semantics are proven across runtimes.
