# Security Controls

aioduct keeps security-sensitive HTTP behavior explicit and feature-scoped. This page summarizes the built-in controls and the areas intentionally left to callers or host platforms.

## Built-In Controls

| Area | Behavior |
| --- | --- |
| HTTP cache | Opt-in `HttpCache` follows cacheable methods/statuses, `Cache-Control`, `Expires`, validators, `Vary`, `stale-while-revalidate`, and `stale-if-error`. Unsafe methods invalidate matching entries. |
| HSTS | Opt-in `HstsStore` records `Strict-Transport-Security` only from HTTPS responses, upgrades later HTTP requests, handles `includeSubDomains`, `max-age=0`, case-insensitive host matching, and host inputs with a single port suffix. |
| Alt-Svc HTTP/3 upgrade | Opt-in `.alt_svc_h3(true)` caches `h3` advertisements by origin, respects `ma`, supports `clear`, and keeps the original request host for TLS SNI when the alternate service uses a different endpoint. |
| Header safety | Request construction uses typed `http` header parsing and rejects invalid header names/values through builder errors. |
| HTTP Message Signatures | `message_signatures` builds RFC 9421 request signature bases, formats `Signature-Input` / `Signature` headers for caller-provided signature bytes, and can automatically sign finalized native request attempts. |
| QUIC/TLS dependency line | HTTP/3 uses the workspace `quinn` dependency through the `http3` and `rustls` feature set; dependency updates are handled through normal Cargo resolution and security review. |
| Proxy/environment settings | Client configuration is an immutable snapshot. Reusing a client keeps its configured proxy/cache/HSTS/Alt-Svc state; rebuild a client to re-read environment proxy variables or apply different proxy policy. |

## Request Signing

RFC 9421 request signature-base generation is available through `MessageSignatureConfig`. Callers choose the signing algorithm, sign the generated base, then attach the formatted `Signature-Input` and `Signature` headers manually on any runtime.

Native tokio, smol, and compio clients can also use `HttpEngineBuilder::message_signature(config, signer)` to sign after default headers, cookies, cache validators, middleware, digest-auth retry headers, forwarding request rewrites, and framing cleanup have finalized each request attempt. Signer errors abort the request rather than sending an unsigned request. Response verification, `Accept-Signature`, multiple-signature merging, async automatic signing, and automatic body digest generation remain Future Work.

## Platform-Managed Runtimes

Browser `wasm` and `wasi-p2` transports delegate parts of caching, TLS verification, and network policy to the host. aioduct still applies API-level request construction and header validation where those runtimes expose the necessary controls.
