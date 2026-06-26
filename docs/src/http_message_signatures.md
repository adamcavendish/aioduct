# HTTP Message Signatures

aioduct provides portable helpers for RFC 9421 HTTP Message Signatures request
signing. This stage builds signature bases and formats `Signature-Input` and
`Signature` header values; callers still choose and run the cryptographic
signing algorithm.

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
    .component(MessageSignatureComponent::Method)
    .component(MessageSignatureComponent::Authority)
    .component(MessageSignatureComponent::Path)
    .component(MessageSignatureComponent::Header {
        name: http::header::DATE,
    })
    .created(1_618_884_473)
    .key_id("test-key");

let base = config.signature_base(&Method::GET, &target_uri, &request_target, &headers)?;
let signature_bytes = my_signing_function(base.as_bytes());
let signature_headers = config.headers_from_signature(signature_bytes)?;
signature_headers.insert_into(&mut headers);
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
| Header fields | Lowercase field names; repeated values are joined with `, `. |

Missing covered headers, duplicate component identifiers, invalid labels,
non-ASCII generated signature bases, and covered header values that cannot be
represented as ASCII header fields return `MessageSignatureError`.

## Manual And Async Signers

For async, host-backed, WebCrypto, KMS, or HSM signing, build the signature base,
await the external signer yourself, then call `headers_from_signature()` with the
returned bytes. This avoids blocking a runtime thread.

The synchronous `MessageSignatureSigner` trait is intended for native automatic
signing in later stages and for local CPU-bound signing. Do not use a blocking
network or device call inside that synchronous signer on an async runtime thread.

## Header Ownership

When automatic signing is not configured, user-supplied `Signature` and
`Signature-Input` headers are ordinary request headers and are preserved. Later
native automatic signing will own those two fields when configured: it will
replace them on each final dispatch so redirects, retries, digest-auth retries,
and stale connection replays cannot send signatures for an earlier request shape.

## Runtime Coverage

The helpers are portable and can be used with native, blocking, wasm, and wasi-p2
request builders by inserting the generated headers manually. Native automatic
request signing is planned separately so it can run after default headers,
cookies, cache validators, redirects, retries, and forwarding have finalized the
request.

Browser Fetch and WASI hosts can still alter or reject some headers at the host
boundary. That host behavior is outside aioduct's control.

## Future Work

- Native automatic request signing.
- Async automatic signer callbacks.
- Response signature verification.
- `Accept-Signature` negotiation.
- Multiple signature dictionary merging.
- `@query-param` and component parameters such as `;sf`, `;key`, `;bs`, and `;tr`.
- Trailer coverage and response `;req` / `@status` components.
- Automatic `Content-Digest` generation for buffered or streaming request bodies.
