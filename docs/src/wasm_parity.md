# WASM/WASI Runtime Parity

This document compares HTTP client capabilities across aioduct's five runtime
backends: **tokio**, **smol**, **compio** (native), **wasm** (browser Fetch API),
and **wasi-p2** (WASI Preview 2 `wasi:http/outgoing-handler`).

Markers:

| Marker | Meaning |
|--------|---------|
| ✓      | Library-supported (with cfg feature cited) |
| ✗      | Not supported |
| ⚠      | Platform-managed (delegated to host runtime) |
| —      | Not applicable |

Lines in [brackets] reference the implementation file and line number for ⚠
claims. Feature flags in backticks cite the cfg gate enabling the capability.

## Feature Comparison

### HTTP Methods

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| GET, POST, HEAD, PUT, DELETE, PATCH | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Custom method | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |

All backends support the six standard methods plus an arbitrary-method
`request()` entry point. WASM: `wasm.rs` lines 37-76. WASI-P2: `wasi_p2.rs`
lines 69-111.

### Request Headers

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Set (per-request) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Override (batch) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Default headers | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |

All backends support setting headers per-request and configuring default headers
on the client builder. WASM: `wasm.rs` lines 185-194 (set), 92-96 (default).
WASI-P2: `wasi_p2.rs` lines 167-176 (set), 38-42 (default).

### Authentication

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Bearer token | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Basic auth | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ✓ `wasi-p2` |
| Digest auth | ✓ | ✓ | ✓ | ✗ | ✗ |
| Netrc | ✓ | ✓ | ✓ | ✗ | ✗ |

WASM: `wasm.rs` `bearer_auth()` at line 203 and `basic_auth()` at line 215 set
the `Authorization` header with Bearer/Basic credentials. `basic_auth()` uses
base64 encoding (matching the native implementation).

WASI-P2: `wasi_p2.rs` `bearer_auth()` at line 197 and `basic_auth()` at line
210 set the `Authorization` header identically.

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
| Form (urlencoded) | ✓ `json` | ✓ `json` | ✓ `json` | ✗ | ✗ |

WASM: `wasm.rs` line 197 accepts `impl Into<Bytes>`. No streaming body or
multipart integration. JSON serialization at lines 218-225 (`cfg(feature =
"json")`).

WASI-P2: `wasi_p2.rs` line 179 accepts `impl Into<Bytes>`. No streaming or
multipart integration. JSON at lines 186-194.

Native runtimes support streaming bodies via `RequestBodySend`, multipart via
the `multipart` module, and form encoding via `serde_urlencoded`.

### Response Body

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| `bytes()` | ✓ | ✓ | ✓ | ✓ `wasm` | ✓ `wasi-p2` |
| `text()` | ✓ | ✓ | ✓ | ✓ `wasm` | ✓ `wasi-p2` |
| `json()` | ✓ `json` | ✓ `json` | ✓ `json` | ✓ `json` | ✓ `json` |
| Streaming (`into_bytes_stream()`) | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✓ `wasm` | ⚠ sync-only [1] |
| SSE (Server-Sent Events) | ✓ | ✓ | ✓ | ✗ | ✗ |

[1] WASI-P2: `wasi_p2.rs` lines 426-453. `into_bytes_stream()` returns
`WasiBodyStream` which uses `blocking_read` internally — the stream's `next()`
is synchronous (line 486: `stream.blocking_read(64 * 1024)`), not truly async.
It blocks the calling thread. This is adequate for single-threaded WASI
environments but not for concurrent workloads.

WASM: `wasm.rs` lines 442-453. `into_bytes_stream()` returns `WasmBodyStream`
which wraps the browser's `ReadableStream` — fully async via `JsFuture`.

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

[2] WASM: `wasm.rs` line 267 creates a `web_sys::Request` via
`new_with_str_and_init`. The `RequestInit` does not set a `redirect` mode —
the browser's default of `follow` applies. The user cannot inspect or control
the redirect count, nor apply a custom policy. The `RedirectPolicy` type
(`redirect.rs`) is portable but not wired into `WasmClient`.

WASI-P2: `wasi_p2.rs` has no redirect handling; the response is returned
as-is. The `RedirectPolicy` type is not integrated.

Native runtimes execute redirects in the client engine (gated behind
`#[cfg(not(target_arch = "wasm32"))]`), configurable via `RedirectPolicy`.

### Cookies

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Cookie jar (store/apply) | ✓ | ✓ | ✓ | ⚠ browser-managed [3] | ✗ |
| Set-Cookie handling | ✓ | ✓ | ✓ | ⚠ browser-managed [3] | ✗ |

[3] WASM: The browser's fetch API handles cookies transparently — `Set-Cookie`
headers are processed and `Cookie` headers are attached automatically. The
`CookieJar` portable module (`cookie/mod.rs`) compiles on WASM and can be used
manually, but it is not integrated into `WasmClient` (no automatic
`store_from_response` or `apply_to_request` calls). Marked ⚠ because cookie
behavior exists but is opaque to the application.

WASI-P2: No cookie jar integration. The `CookieJar` type is available as a
portable module for manual use.

### Timeout

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Request timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ AbortController [4] | ⚠ WASI-mapped [5] |
| Connect timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ | ⚠ WASI-mapped [5] |
| Read timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ | ⚠ WASI-mapped [5] |

[4] WASM: `wasm.rs` lines 258-264 create an `AbortController` and attach its
signal to the `RequestInit`. A `setTimeout` callback (lines 276-288 for Window,
296-310 for Worker) fires `controller.abort()` after the timeout duration
elapses. This is a combined request-level timeout; finer-grained connect/read
timeouts are not available.

[5] WASI-P2: `wasi_p2.rs` lines 287-292. The user's timeout `Duration` is
converted to nanoseconds and passed to all three WASI `RequestOptions` fields:
`connect_timeout`, `first_byte_timeout`, and `between_bytes_timeout`.
Enforcement is delegated to the WASI runtime (e.g., wasmtime).

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

[6] WASM: The browser resolves DNS internally. The `web_sys::Request` /
`fetch()` interface provides no DNS configuration hooks. `wasi_p2.rs` module
doc comment (line 4-5): "TLS, connection pooling, and DNS resolution are
handled transparently by the WASI runtime."

[7] WASI-P2: DNS is resolved by the WASI runtime (e.g., wasmtime). The
`wasi:http/outgoing-handler` does not expose DNS resolver configuration.
`wasi_p2.rs` lines 4-5 document this.

### TLS

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| rustls | ✓ `rustls` | ✓ `rustls` | ✓ `rustls` | ⚠ browser-managed [8] | ⚠ WASI-managed [9] |
| Platform-native certs | ✓ `rustls-native-roots` | ✓ `rustls-native-roots` | ✓ `rustls-native-roots` | — | — |
| Client certificates | ✓ `rustls` | ✓ `rustls` | ✓ `rustls` | ✗ | ✗ |

[8] WASM: The browser's fetch API handles TLS negotiation automatically. No TLS
configuration is exposed. `lib.rs` line 122-123: `tls` module gated behind
`#[cfg(not(target_arch = "wasm32"))]`.

[9] WASI-P2: The WASI runtime manages TLS. `wasi_p2.rs` module doc comment
(lines 4-5): "TLS, connection pooling, and DNS resolution are handled
transparently by the WASI runtime." No client-certificate or TLS-version
configuration is available.

### Connection Pooling

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| Keep-alive | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |
| max_idle_per_host | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |
| idle_timeout | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |
| max_lifetime | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ⚠ browser-managed [10] | ⚠ WASI-managed [11] |

[10] WASM: The browser manages HTTP connection pools internally. No pool
configuration is exposed. `lib.rs` line 104-105: `pool` module gated behind
`#[cfg(not(target_arch = "wasm32"))]`.

[11] WASI-P2: The WASI runtime manages connection pooling. `wasi_p2.rs` lines
4-5 document this. No pool knobs are available.

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
per-codec configuration is possible.

WASI-P2: No decompression integration. The `decompress.rs` module compiles
but is not called by `WasiClient`. Users can apply the portable
`DecompressBody` type manually.

Native runtimes integrate `maybe_decompress()` from `decompress.rs` into the
response body pipeline. Each codec is behind a cfg feature: `gzip`, `brotli`,
`zstd`, `deflate`.

### HTTP/2 and HTTP/3

| Feature | tokio | smol | compio | wasm | wasi-p2 |
|---------|-------|------|--------|------|---------|
| HTTP/2 | ✓ | ✓ | ✓ | ⚠ browser-managed [13] | ⚠ WASI-managed [14] |
| HTTP/2 config tuning | ✓ `tokio` | ✓ `smol` | ✓ `compio` | ✗ | ✗ |
| HTTP/3 | ✓ `http3` | ✓ `http3` | ✓ `http3` | ⚠ browser-managed [13] | ⚠ WASI-managed [14] |

[13] WASM: The browser negotiates HTTP/2 and HTTP/3 via ALPN. No version
selection or tuning is exposed by the fetch API. The `Http2Config` type
(`http2.rs`) is portable but its `apply()` method is gated behind
`#[cfg(not(target_arch = "wasm32"))]` (line 119).

[14] WASI-P2: The WASI runtime negotiates the HTTP version. No version
configuration is exposed. `Http2Config` is not applicable.

Native runtimes negotiate HTTP/2 through hyper's `http2` builder. HTTP/3 is
enabled via the `http3` feature (requires `rustls`; see `lib.rs` line 23-24).

## Why Features Are Platform-Managed on WASM and WASI-P2

### WASM (browser)

The browser WASM client (`wasm.rs`) delegates all networking to the
**browser's Fetch API** (`web_sys::Request` / `window.fetch()`). This means:

- **TLS, DNS, HTTP/2, connection pooling**: These are internal to the browser's
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
  away. `wasi_p2.rs` lines 4-5: "TLS, connection pooling, and DNS resolution
  are handled transparently by the WASI runtime (e.g., wasmtime)."
  Configuration depends entirely on the host runtime.

- **Redirects and cookies**: `outgoing-handler` does not include redirect
  following or cookie management. These must be implemented in the client, but
  the current `WasiClient` has not yet wired in the portable `RedirectPolicy`
  or `CookieJar` types.

- **Timeout**: Timeout values are passed through to the WASI runtime via
  `RequestOptions` (lines 287-292). Whether they are honored depends on the
  runtime implementation.

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

| Capability area | Native (tokio/smol/compio) | WASM (browser) | WASI-P2 |
|-----------------|---------------------------|----------------|---------|
| Request/response basics | Fully supported | Fully supported | Fully supported |
| Streaming body | Full async streaming | Response streaming via ReadableStream | Sync-only body stream |
| Redirect, cookie, retry, middleware | Integrated | Not applicable / platform-managed | Types available, not integrated |
| TLS, DNS, pooling, HTTP version | Configurable | Browser-managed | WASI runtime-managed |
| Proxy | Full support | Not available | Not available |
| Compression | Per-codec cfg features | Browser-managed | Not integrated |
