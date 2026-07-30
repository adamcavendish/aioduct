# WASM/WASI Runtime Parity

This document compares HTTP client capabilities across aioduct's five runtime
backends: **tokio**, **smol**, **compio** (native), **wasm** (compatible host
Fetch API), and **wasi-p2** (WASI Preview 2 `wasi:http/outgoing-handler`).

Markers:

| Marker | Meaning |
|--------|---------|
| ✓      | Library-supported (with cfg feature cited) |
| ✗      | Not available |
| ⚠      | Platform-managed (delegated to host runtime) |
| —      | Not applicable |

Numbered footnotes explain which platform owns each ⚠ capability and name the
relevant implementation module or symbol. Feature flags in backticks cite the
cfg gate enabling the capability.

## Feature Comparison

### HTTP Methods

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| GET, POST, HEAD, PUT, DELETE, PATCH | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Custom method | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |

All backends support the six standard methods plus an arbitrary-method
`request()` entry point through `WasmClient` or `WasiClient`.

### Request Headers

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Set (per-request) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Override (batch) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Default headers | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |

All backends support setting headers per request and configuring default
headers on their client builder. The platform implementations live on
`WasmRequestBuilder` and `WasiRequestBuilder`.

### HTTP Message Signatures

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| RFC 9421 request/response signature-base helpers | ✓ | ✓ | ✓ | ✓ | ✓ |
| RFC 9421 request/response verification policy | ✓ | ✓ | ✓ | ✓ | ✓ |
| RFC 9421 `Accept-Signature` parser/builder/fulfillment configs | ✓ | ✓ | ✓ | ✓ | ✓ |
| RFC 9421 caller-supplied `;tr` trailer field components | ✓ | ✓ | ✓ | ✓ | ✓ |
| Covered `Content-Digest` verification with caller-supplied body bytes | ✓ | ✓ | ✓ | ✓ | ✓ |
| SHA-256 `Content-Digest` value helpers for explicit headers | ✓ | ✓ | ✓ | ✓ | ✓ |
| Automatic buffered request `Content-Digest` generation | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ manual headers only | ✗ manual headers only |
| Forward bounded response `Content-Digest` generation | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ no native forwarding | ✗ no native forwarding |
| Sync automatic request signing | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ manual headers only | ✗ manual headers only |
| Async automatic request signing | ✓ `Send` future | ✓ `Send` future | ✓ local future | ✗ manual headers only | ✗ manual headers only |
| Forward automatic response signing | ✓ sync/`Send` async | ✓ sync/`Send` async | ✓ sync/local async | ✗ no native forwarding | ✗ no native forwarding |
| Automatic trailer-based digest/signature generation | ✗ manual contexts only | ✗ manual contexts only | ✗ manual contexts only | ✗ manual contexts only | ✗ manual contexts only |

`message_signatures` is portable: callers can build request or response
signature bases, sign them with their own key material, verify signed request or
response headers with caller-owned cryptography, parse or build
`Accept-Signature`, convert accepted entries into signing configs, cover
caller-supplied trailer fields with `;tr`, verify covered SHA-256
`Content-Digest` fields when callers attach body bytes to verification contexts,
format SHA-256 `Content-Digest` values for explicit headers, and attach
`Signature-Input` / `Signature` through the normal header APIs on every runtime.
Native clients can also generate SHA-256 `Content-Digest` for buffered request
bodies before signing. Native automatic request signing supports sync signers
plus async send-runtime signers for tokio/smol and async local-runtime signing
futures for compio. It runs after default headers, cookies, cache validators,
middleware, digest-auth retry headers, forwarding request rewrites, framing
cleanup, and digest insertion have finalized each request attempt. Native forward
builders can also buffer downstream responses up to a caller cap to generate
`Content-Digest` before signing downstream responses after response cleanup and
`on_response`; this is not available on browser Fetch or WASI because those
targets do not expose the native forwarding builder. Browser Fetch and WASI
request dispatch do not expose automatic digest/signing hooks; use the portable
helper flow and manual headers there. Automatic trailer-based digest or
signature generation is not exposed on any runtime: native HTTP/1 and HTTP/2 can
carry request trailer frames, but native HTTP/3 streams request bodies while
failing closed if either direction emits trailers, browser Fetch and WASI do not
expose matching request-trailer hooks, and forward response signing runs before
the downstream response body is streamed. Use caller-supplied trailer maps with
the portable context APIs instead.

### Authentication

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Bearer token | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Basic auth | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Digest auth | ✓ | ✓ | ✓ | ✗ | ✗ |
| Netrc | ✓ | ✓ | ✓ | ✗ | ✗ |

`WasmRequestBuilder` and `WasiRequestBuilder` implement `bearer_auth()` and
`basic_auth()` by setting the `Authorization` header. `basic_auth()` uses
base64 encoding, matching the native implementation.

Digest auth and netrc-based auth are not integrated into WASM or WASI-P2 clients
— the portable types compile but are not wired into the request flow.

### URL Query Parameters

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| `query()` (string pairs) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| `query_serde()` (serialize) | ✓ `json` | ✓ `json` | ✓ `json` | ✗ | ✗ |

WASM: `wasm.rs` `query()` percent-encodes key/value pairs and appends them to
the request URI, matching the native implementation. `query_serde()` is not
available — the `serde_urlencoded` crate is not a dependency in the wasm target
configuration.

WASI-P2: `wasi_p2.rs` `query()` percent-encodes and appends pairs identically.
`query_serde()` is likewise not available.

### Request Body

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Buffered (`body()`) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| JSON (`json()`) | ✓ `json` | ✓ `json` | ✓ `json` | ✓ `json` | ✓ `json` |
| Streaming | ✓ | ✓ | ✓ | ✗ | ✗ |
| Multipart/form-data | ✓ | ✓ | ✓ | ✗ | ✗ |
| Form (urlencoded string pairs) | ✓ | ✓ | ✓ | ✓ `wasm` | ✓ `wasi-p2` |
| Form (serializable value) | ✓ `json` | ✓ `json` | ✓ `json` | ✗ | ✗ |

WASM: `wasm.rs` accepts `impl Into<Bytes>` via `body()` and URL-encoded string
forms via `form()`. No streaming body or multipart integration. JSON
serialization is available with `cfg(feature = "json")`.

WASI-P2: `wasi_p2.rs` accepts `impl Into<Bytes>` via `body()` and URL-encoded
string forms via `form()`. No streaming or multipart integration. JSON is
available with `cfg(feature = "json")`.

Native runtimes support streaming bodies via `RequestBodySend` or
`RequestBodyLocal`, multipart via the `multipart` module, string-pair form
encoding, and serializable form values through `serde_urlencoded`.

### Response Body

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| `bytes()` | ✓ | ✓ | ✓ | ✓ `wasm` | ✓ `wasi-p2` |
| `text()` | ✓ | ✓ | ✓ | ✓ `wasm` | ✓ `wasi-p2` |
| `json()` | ✓ `json` | ✓ `json` | ✓ `json` | ✓ `json` | ✓ `json` |
| Streaming (`into_bytes_stream()`) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ⚠ sync-only [1] |
| SSE (Server-Sent Events) | ✓ | ✓ | ✓ | ✗ | ✗ |

[1] WASI-P2: `WasiResponse::into_bytes_stream()` returns `WasiBodyStream`,
which uses the WASI input stream's `blocking_read` operation internally. Its
`next()` operation blocks the calling thread and is not an asynchronous poll.
This is adequate for simple single-threaded WASI guests but not for concurrent
workloads.

WASM: `WasmResponse::into_bytes_stream()` returns `WasmBodyStream`, which wraps
the browser's `ReadableStream` and waits asynchronously through `JsFuture`.

SSE: The `SseDecoder` (portable) can parse event streams from raw bytes on any
target. However, the streaming `SseStream<B>` type requires `B: Body<Data =
Bytes, Error = Error>`, which the WASM/WASI body stream types do not implement.
Feed bytes through `SseDecoder` manually on these targets.

### Redirects

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Follow | ✓ | ✓ | ✓ | ⚠ browser-managed [2] | ✗ |
| Max redirects | ✓ | ✓ | ✓ | ⚠ browser-managed [2] | ✗ |
| Custom policy | ✓ | ✓ | ✓ | ✗ | ✗ |

[2] WASM: `WasmRequestBuilder::send()` creates a `web_sys::Request` without
overriding `RequestInit.redirect`, so the browser's default `follow` mode
applies. The user cannot inspect or control the redirect count or apply a
custom policy. The portable `RedirectPolicy` type is not wired into
`WasmClient`.

WASI-P2: `wasi_p2.rs` has no redirect handling; the response is returned
as-is. The `RedirectPolicy` type is not integrated.

Native runtimes execute redirects in the client engine (gated behind
`#[cfg(not(target_arch = "wasm32"))]`), configurable via `RedirectPolicy`.

### Cookies

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Cookie jar (store/apply) | ✓ | ✓ | ✓ | ⚠ browser-managed [3] | ✗ |
| Set-Cookie handling | ✓ | ✓ | ✓ | ⚠ browser-managed [3] | ✗ |

[3] WASM: Cookie handling follows the browser's Fetch credentials and CORS
policy. Same-origin cookies use the browser-managed cookie store by default;
cross-origin behavior depends on browser policy, and `WasmClient` does not
currently expose a credentials-mode control. The portable `CookieJar` module
compiles on WASM but is not integrated into `WasmClient`, and forbidden
`Set-Cookie` response fields are not exposed to application code.

WASI-P2: No cookie jar integration. The `CookieJar` type is available as a
portable module for manual use.

### Timeout

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Request timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ AbortController [4] | ⚠ WASI-mapped [5] |
| Connect timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ explicit error [4] | ⚠ WASI-mapped [5] |
| Read timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ explicit error [4] | ⚠ WASI-mapped [5] |

[4] WASM: `WasmRequestBuilder::send()` creates an `AbortController`, attaches
its signal to `RequestInit`, and schedules `controller.abort()` through the
available Window or Worker timer API. This is one request-level timeout;
finer-grained connect/read timeouts are unavailable. Calling those builder
controls records an unsupported-operation error returned by `send()`.

[5] WASI-P2: the user's request timeout is converted to nanoseconds and passed
to all three WASI `RequestOptions` fields: `connect_timeout`,
`first_byte_timeout`, and `between_bytes_timeout`. Per-request
`connect_timeout()` overrides the connect field, and `read_timeout()` maps to
the `between_bytes_timeout` field. Enforcement is delegated to the WASI runtime
(e.g., wasmtime).

### Proxy

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| HTTP proxy | ✓ | ✓ | ✓ | ✗ | ✗ |
| HTTPS proxy | ✓ `rustls` | ✓ `rustls` | ✓ `rustls` | ✗ | ✗ |
| SOCKS proxy | ✓ | ✓ | ✓ | ✗ | ✗ |
| System proxy | ✓ | ✓ | ✓ | ✗ | ✗ |

WASM: The browser Fetch API does not expose proxy configuration to JavaScript.
Proxies must be configured at the browser or OS level. The `proxy` module
types are portable but not applicable.

WASI-P2: The `wasi:http/outgoing-handler` interface has no proxy concept.
The `proxy` module types compile but are not integrated.

### DNS

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Custom resolver | ✓ `hickory-dns` | ✓ `hickory-dns` | ✓ `hickory-dns` | ⚠ browser-managed [6] | ⚠ WASI-managed [7] |
| System resolver | ✓ | ✓ | ✓ | ⚠ browser-managed [6] | ⚠ WASI-managed [7] |

[6] WASM: The browser resolves DNS internally. The `web_sys::Request` and
`fetch()` interfaces provide no DNS configuration hooks.

[7] WASI-P2: DNS is resolved by the WASI runtime, such as Wasmtime. The
`wasi:http/outgoing-handler` interface does not expose DNS resolver
configuration.

### TLS

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| rustls | ✓ `rustls` | ✓ `rustls` | ✓ `rustls` | ⚠ browser-managed [8] | ⚠ WASI-managed [9] |
| Platform-native certs | ✓ `rustls-native-roots` | ✓ `rustls-native-roots` | ✓ `rustls-native-roots` | — | — |
| Client certificates | ✓ `rustls` | ✓ `rustls` | ✓ `rustls` | ✗ | ✗ |

[8] WASM: The browser's Fetch API handles TLS negotiation automatically. No TLS
configuration is exposed, and the native `tls` module is unavailable on
`wasm32`.

[9] WASI-P2: The WASI runtime manages TLS through
`wasi:http/outgoing-handler`. No client-certificate or TLS-version
configuration is available through `WasiClient`.

### Connection Pooling

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Keep-alive | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |
| max_idle_per_host | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |
| idle_timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |
| max_lifetime | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |

[10] WASM: The browser manages HTTP connection pools internally. No pool
configuration is exposed, and the native pool is unavailable on `wasm32`.

[11] WASI-P2: The WASI runtime manages connection pooling behind
`wasi:http/outgoing-handler`; `WasiClient` exposes no pool controls.

### Retry

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Retry config (max, backoff) | ✓ | ✓ | ✓ | ✗ | ✗ |
| Retry budget | ✓ | ✓ | ✓ | ✗ | ✗ |
| Retry-After parsing | ✓ | ✓ | ✓ | ✗ | ✗ |

WASM + WASI-P2: The `RetryConfig`, `RetryBudget`, and `parse_retry_after`
types (`retry.rs`) are portable and compile on all targets. However, they are
not integrated into `WasmClient` or `WasiClient` — neither client has a retry
loop. Native runtimes integrate retry in the client engine
(`#[cfg(not(target_arch = "wasm32"))]`).

### Middleware

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| `on_request` | ✓ | ✓ | ✓ | ✗ | ✗ |
| `on_response` | ✓ | ✓ | ✓ | ✗ | ✗ |
| `on_error` | ✓ | ✓ | ✓ | ✗ | ✗ |
| `on_redirect` | ✓ | ✓ | ✓ | ✗ | ✗ |
| `on_retry` | ✓ | ✓ | ✓ | ✗ | ✗ |

WASM + WASI-P2: The `Middleware` trait and `MiddlewareStack` (`middleware.rs`)
are portable and compile on all targets. However, they are not integrated into
`WasmClient` or `WasiClient` — neither client exposes a middleware push API or
applies the stack during request/response processing.

### Compression

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| gzip | ✓ `gzip` | ✓ `gzip` | ✓ `gzip` | ⚠ browser-managed [12] | ✗ |
| brotli | ✓ `brotli` | ✓ `brotli` | ✓ `brotli` | ⚠ browser-managed [12] | ✗ |
| zstd | ✓ `zstd` | ✓ `zstd` | ✓ `zstd` | ⚠ browser-managed [12] | ✗ |
| deflate | ✓ `deflate` | ✓ `deflate` | ✓ `deflate` | ⚠ browser-managed [12] | ✗ |

[12] WASM: The browser fetch API automatically sets `Accept-Encoding` and
decompresses response bodies. The `decompress.rs` module is portable but not
wired into `WasmClient` — the browser handles it transparently. No
per-codec configuration is possible. Calling `no_decompression()` records an
unsupported-operation error returned by `send()`.

WASI-P2: No decompression integration. The `decompress.rs` module compiles
but is not called by `WasiClient`; `no_decompression()` is therefore already
satisfied because the client does not add `Accept-Encoding` or decode response
bodies. Users can apply the portable `DecompressBody` type manually.

Native runtimes integrate `maybe_decompress()` from `decompress.rs` into the
response body pipeline. Each codec is behind a cfg feature: `gzip`, `brotli`,
`zstd`, `deflate`.

### HTTP/2 and HTTP/3

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| HTTP/2 | ✓ | ✓ | ✓ | ⚠ browser-managed [13] | ⚠ WASI-managed [14] |
| HTTP/2 config tuning | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ | ✗ |
| HTTP/3 | ✓ `http3` | ✗ | ✗ | ⚠ browser-managed [13] | ⚠ WASI-managed [14] |

[13] WASM: The browser negotiates HTTP/2 and HTTP/3 via ALPN. No version
selection or tuning is exposed by the fetch API. The `Http2Config` type
(`http2.rs`) is portable but its `apply()` method is gated behind
`#[cfg(not(target_arch = "wasm32"))]`. Calling request-builder
`version()` records an unsupported-operation error returned by `send()`.

[14] WASI-P2: The WASI runtime negotiates the HTTP version. No version
configuration is exposed. `Http2Config` is not applicable, and request-builder
`version()` records an unsupported-operation error returned by `send()`.

Native runtimes negotiate HTTP/2 through hyper's `http2` builder. Native
HTTP/3 is Tokio-only and is enabled via the `http3` feature, which requires
`rustls`.

## Why Features Are Platform-Managed on WASM and WASI-P2

### WASM (host Fetch API)

The WASM client (`wasm.rs`) delegates networking to a compatible host's Fetch
API (`web_sys::Request` / `globalThis.fetch()`). Browser and worker runtimes
share this transport entry point, while browser-specific policy still follows
the browser Fetch implementation. This means:

- **TLS, DNS, HTTP/2, connection pooling**: These are internal to the host's
  network stack. The Fetch API provides no configuration hooks for any of
  these. The `WasmClient` literally cannot influence them.

- **Cookies and redirects**: The browser processes `Set-Cookie` and follows
  redirects automatically as part of the fetch spec. Browser Fetch filters
  forbidden response headers (including `Set-Cookie`) from the `Headers`
  object, so `Set-Cookie` is not readable from `WasmClient::headers()` or
  `CookieJar` integration. The client has no way to suppress browser-level
  redirect following via the current implementation.

- **Timeout**: There is no native timeout API in fetch. The `WasmClient`
  emulates timeouts using `AbortController` + `setTimeout`, which aborts the
  request after the configured duration. This is a combined request-level
  timeout — finer-grained connect/read timeouts are not available.

- **Compression**: Browsers always send `Accept-Encoding` and transparently
  decompress responses. The client receives already-decompressed bytes. The
  `decompress.rs` module would be redundant if applied.

### WASI-P2 (wasi:http/outgoing-handler)

The WASI-P2 client (`wasi_p2.rs`) uses the **`wasi:http/outgoing-handler`**
interface. This interface is intentionally high-level:

- **TLS, DNS, connection pooling**: The WASI component model abstracts these
  away behind `wasi:http/outgoing-handler`. Configuration depends entirely on
  the host runtime.

- **Redirects and cookies**: `outgoing-handler` does not include redirect
  following or cookie management. These must be implemented in the client, but
  the current `WasiClient` has not yet wired in the portable `RedirectPolicy`
  or `CookieJar` types.

- **Timeout**: Timeout values are passed through to the WASI runtime via
  `RequestOptions`. Whether they are honored depends on the runtime
  implementation.

- **Streaming response body**: The WASI-P2 `InputStream` uses `blocking_read`,
  which is synchronous. This is consistent with the WASI Preview 2 model but
  means the `WasiBodyStream` does not support non-blocking iteration.

### Native (tokio/smol/compio)

Native runtimes use aioduct's full client engine (`HttpEngineSend` /
`HttpEngineLocal`), which directly manages **hyper connections**, **rustls TLS
sessions**, **connection pools**, **DNS resolution**, and **HTTP redirect
loops**. All features listed above are available because the client owns the
full networking stack.

## Summary

| Capability area | Native (tokio/smol/compio) | WASM (host Fetch) | WASI-P2 |
|-----------------|---------------------------|----------------|---------|
| Request/response basics | Fully supported | Fully supported | Fully supported |
| Streaming body | Full async streaming | Response streaming via ReadableStream | Sync-only body stream |
| Redirect, cookie, retry, middleware | Integrated | Not applicable / platform-managed | Types available, not integrated |
| TLS, DNS, pooling, HTTP version | Configurable | Host-managed | WASI runtime-managed |
| Proxy | Full support | Not available | Not available |
| Compression | Per-codec cfg features | Host-managed | Not integrated |

## Wasmtime Host Adapter

`aioduct::WasiClient` is the guest-side WASI-P2 client. It cannot and should
not carry host trust policy such as allowed origins, CA roots, insecure
certificate mode, secret header injection, body limits, or redacted diagnostics.

Hosts embedding Wasmtime components can enable the first-party `wasmtime`
feature and use `aioduct::wasmtime` to install a `wasi:http` hook. That hook
validates a guest request with host-owned policy, injects host-owned headers
after validation, and forwards the request through a native `aioduct`
transport.

The host transport line covers `RuntimePoll` native clients (`TokioClient` and
`SmolClient`) directly. `CompioClient` is covered through
`CompioHostTransport`, which owns the local-runtime worker and bounded body
bridge needed for non-`Send` compio state. Browser `wasm` has no Wasmtime host
hook; it remains browser Fetch managed.

This narrows the WASI-P2 host-side policy gap for Wasmtime embeddings. It does
not change the guest `aioduct::WasiClient` API and it does not make browser
`wasm` host-policy configurable.
