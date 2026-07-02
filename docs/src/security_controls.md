# Security Controls

aioduct keeps security-sensitive HTTP behavior explicit and feature-scoped. This page summarizes the built-in controls and the areas intentionally left to callers or host platforms.

## Built-In Controls

| Area | Behavior |
| --- | --- |
| HTTP cache | Opt-in `HttpCache` follows cacheable methods/statuses, `Cache-Control`, `Expires`, validators, `Vary`, `stale-while-revalidate`, and `stale-if-error`. Unsafe methods invalidate matching entries. |
| HSTS | Opt-in `HstsStore` records `Strict-Transport-Security` only from HTTPS responses, upgrades later HTTP requests, handles `includeSubDomains`, `max-age=0`, case-insensitive host matching, and host inputs with a single port suffix. |
| Alt-Svc HTTP/3 upgrade | Opt-in `.alt_svc_h3(true)` caches `h3` advertisements by origin, respects `ma`, supports `clear`, and keeps the original request host for TLS SNI when the alternate service uses a different endpoint. |
| Header safety | Request construction uses typed `http` header parsing and rejects invalid header names/values through builder errors. Cross-origin redirects strip built-in credential headers plus request headers whose values are marked sensitive. |
| HTTP Message Signatures | `message_signatures` builds RFC 9421 request and response signature bases, covers caller-supplied trailer fields with `;tr`, formats `Signature-Input` / `Signature` headers for caller-provided signature bytes, parses and formats `Accept-Signature`, converts accepted signature requests into concrete signing configs, applies verification policy checks, verifies covered SHA-256 `Content-Digest` fields when body bytes are attached, can generate buffered request and bounded forward response SHA-256 `Content-Digest` fields, automatically signs finalized native request attempts, and can automatically sign forwarded downstream responses. |
| QUIC/TLS dependency line | HTTP/3 uses the workspace `quinn` dependency through the `http3` and `rustls` feature set; dependency updates are handled through normal Cargo resolution and security review. |
| Proxy/environment settings | Client configuration is an immutable snapshot. Reusing a client keeps its configured proxy/cache/HSTS/Alt-Svc state; rebuild a client to re-read environment proxy variables or apply different proxy policy. |

## Request Signing

RFC 9421 request and response signature-base generation is available through `MessageSignatureConfig`. Callers choose the signing algorithm, sign the generated base, then attach the formatted `Signature-Input` and `Signature` headers manually on any runtime. Response bases can cover `@status`, caller-supplied trailer fields with `;tr`, and related request components with `;req`.

For incoming signed messages, `MessageSignatureVerificationPolicy` parses a selected request or response signature, enforces required covered components, accepted `alg` and `keyid` metadata, timestamp expiry, clock skew, and maximum signature age, verifies covered SHA-256 `Content-Digest` fields when body bytes are attached, then calls the caller-owned verifier with the rebuilt base and decoded signature bytes. Response verification can bind selected related request components with `;req`; trailer components require caller-attached trailer maps. If a selected signature carries `created` or `expires`, the policy requires a configured validation time and fails closed without one. Callers still own cryptographic verification and trust decisions.

`AcceptSignature` parses and formats requested signature dictionaries, validates whether covered components target a request, response, or response with related request, and turns accepted entries into concrete `MessageSignatureConfig` values. Fulfillment remains explicit: callers own request selection, key selection, timestamp generation, cryptography, and attaching the resulting `Signature-Input` / `Signature` fields.

Native tokio, smol, and compio clients can also use `HttpEngineBuilder::automatic_content_digest(true)` to insert SHA-256 `Content-Digest` for buffered bodies, then `HttpEngineBuilder::message_signature(config, signer)` for sync signing, `message_signature_async(config, signer)` for send-runtime async signing, or `message_signature_async_local(config, signer)` for local-runtime async signing after default headers, cookies, cache validators, middleware, digest-auth retry headers, forwarding request rewrites, and framing cleanup have finalized each request attempt. Existing `Content-Digest` headers are preserved. Streaming and middleware-replaced bodies are not buffered automatically; set `Content-Digest` explicitly for those requests, using the SHA-256 value helpers when useful. Signer errors abort the request rather than sending an unsigned request.

Forward builders can generate a bounded downstream response `Content-Digest` and
sign the downstream response with sync or async signers after upstream response
hop-by-hop cleanup and `on_response`. Related-request components bind the inbound
request snapshot, not the rewritten upstream request. Response finalization is
fail-closed for signer errors, malformed existing signature dictionaries,
unsupported trailer components, response digest bodies over the configured cap,
`CONNECT`, known upgrade requests, and HTTP/1.1 `101 Switching Protocols`
responses. Synthesized response digest fields are skipped for bodyless responses
such as `HEAD`, `204`, `205`, and `304`. Automatic trailer generation remains
Future Work.

## Platform-Managed Runtimes

Browser `wasm` and `wasi-p2` transports delegate parts of caching, TLS verification, and network policy to the host. aioduct still applies API-level request construction and header validation where those runtimes expose the necessary controls.
