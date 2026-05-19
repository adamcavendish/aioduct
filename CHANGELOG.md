# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0-alpha.1] - 2026-05-19

### Breaking Changes
- Renamed `Client<R>` → `HttpEngineSend<R, C>` (poll-based runtimes) and `HttpEngineLocal<R, C>` (completion-based runtimes)
- Monolithic `Runtime` trait split into `RuntimeCompletion`, `RuntimePoll`, `RuntimeLocal`
- Networking extracted from runtime trait into `ConnectorSend` / `ConnectorLocal` traits — connector is now passed at construction time
- `RequestBuilder` split into `RequestBuilderSend` and `RequestBuilderLocal`
- `ResponseBody` split into `ResponseBodySend` and `ResponseBodyLocal`
- WASM/WASI response body methods are now async and fallible (lazy streaming instead of eager buffering)
- `ResponseExt` trait gains `type ByteStream`, `json()`, `into_bytes_stream()`
- `Connector` trait renamed to `ConnectorLocal`

### Added
- Type aliases: `TokioClient`, `SmolClient`, `CompioClient`, `WasmClient`, `WasiClient`
- Engine aliases: `TokioEngine`, `SmolEngine`, `CompioEngine`
- Blocking wrappers: `BlockingTokioClient`, `BlockingSmolClient`, `BlockingCompioClient`
- Portable traits: `HttpClient`, `RequestBuilderExt`, `ResponseExt`, `ByteStreamExt`
- WASM/WASI lazy body streaming with `WasmBodyStream` and `WasiBodyStream`
- Deferred H1 connection pool check-in — connections return to pool only after response body is fully consumed
- H2 multiplex-wait path — concurrent requests to the same origin wait for in-flight H2 handshake instead of opening redundant connections
- AdaptiveH2c fallback — probes h2c, falls back to h1 if rejected, caches result per origin
- `pool_max_idle_per_host(0)` disables connection pooling entirely
- Happy Eyeballs (RFC 8305) — parallel dual-stack connection racing
- `HttpClient` trait for runtime-agnostic client code
- `tower` feature gate with `connector_layer` / `connector_layer_local` for Tower service integration

### Fixed
- Pool checkout returns stale connections when idle timeout expired
- H2 multiplex clones not evicted when underlying transport breaks
- Connection `upgrade()` handle consumed before caller can use it
- `Connection: close` header not respected — connection was returned to pool
- TLS H1 upgrade path did not apply socket config
- Pool reaper panic when spawned outside async runtime context
- Decompression busy-spin consuming 100% CPU on slow streams
- SOCKS proxy timeout not propagated to handshake phase
- Middleware body propagation losing streamed chunks
- Cookie and cache correctness issues (#136, #139, #152, #123)
- H3 body streaming and trailer handling (#130, #125)
- WASM timeout leak on dropped futures (#173)
- Cache body buffering bypassing `read_timeout` and `bandwidth_limiter` (#169)
- `bytes_sent` metric reporting 0 for streaming request bodies (#184)
- Stale connection retry now preserves middleware-injected headers (#207)
- AdaptiveH2c fallback missing socket config and wrong `remote_addr` (#208, #209)
- Chunk download rejecting valid 200 OK responses
- Numerous additional correctness and security fixes (batches 2–10)

## [0.1.10] - 2026-05-11

### Fixed
- Stale connection retry now works for buffered POST bodies — non-streaming request bodies are transparently replayed when a pooled connection turns out to be stale, fixing "connection closed" errors for POST+JSON requests that previously only retried for empty-body methods (GET, HEAD, DELETE)

## [0.1.9] - 2026-05-10

### Fixed
- Transparent retry on stale pool connections — when a reused connection fails with RST, FIN, GOAWAY, or "connection closed" during `send_request`, the client automatically retries once on a fresh connection for empty-body requests (GET, HEAD, DELETE, etc.)

### Added
- `Error::is_closed()` public API — returns `true` when the error indicates a reused connection was closed by the peer (TCP RST/FIN, HTTP/2 GOAWAY, hyper canceled/incomplete)

## [0.1.8] - 2026-05-09

### Added
- HTTP/3 0-RTT (early data) support — opt-in via `ClientBuilder::h3_zero_rtt(true)`, used only for idempotent methods (GET, HEAD, OPTIONS) with automatic fallback to full handshake on rejection
- Connection coalescing (RFC 7540 §9.1.1) — reuses existing h2/h3 connections whose TLS certificate SANs cover the target domain and whose remote IP matches DNS resolution, matching browser behavior
- DNS-over-HTTPS via `ClientBuilder::dns_over_https(server_ip, server_name)` — encrypted name resolution through DoH endpoints (requires `doh` feature)
- DNS-over-TLS via `ClientBuilder::dns_over_tls(server_ip, server_name)` — encrypted name resolution through DoT endpoints (requires `dot` feature)
- Bandwidth limiter auto-wired into response bodies and aria download engine with per-client isolation
- Interactive landing page with live WASM demo at project GitHub Pages

### Fixed
- Enable `socket2/all` feature unconditionally — `TcpKeepalive::with_retries()` and `SockRef::bind_device()` now compile on smol-only and compio-only builds (tokio was enabling it transitively, masking the issue)
- Include tests in published crate to silence cargo packaging warnings

### Changed
- CI: added isolated-runtime clippy job that checks tokio, smol, and compio individually to catch transitive-dep masking
- CI: pages workflow skips wasm-target snippets during native compilation check
- Deps: bump hickory-proto and hickory-net from 0.26.0 to 0.26.1

## [0.1.7] - 2026-05-04

### Added
- Per-forward h2c mode via `ForwardBuilder::h2c()` — force HTTP/2 prior knowledge on individual forwards without setting `http2_prior_knowledge()` on the entire client
- Adaptive h2c fallback via `ForwardBuilder::adaptive_h2c()` — probes whether the upstream speaks h2c, falls back to HTTP/1.1 on failure, and caches the result per-authority so subsequent requests skip the probe
- `H2cProbeCache` with configurable TTL for per-authority h2c capability caching
- Ergonomic HTTP/2 settings on `ClientBuilder`: `http2_initial_stream_window_size`, `http2_initial_connection_window_size`, `http2_max_frame_size`, `http2_adaptive_window`, `http2_keep_alive_interval`, `http2_keep_alive_timeout`, `http2_keep_alive_while_idle`, `http2_max_header_list_size`, `http2_max_send_buf_size`, `http2_max_concurrent_reset_streams`
- `ClientBuilder::h2c_probe_ttl(duration)` for configuring adaptive h2c cache expiry
- `ProtocolHint` pool key discriminator — h2c connections are pooled separately from h1, preventing protocol mismatch on reuse
- SSE parser rewritten for full WHATWG spec compliance — correct field parsing, multi-line `data`, BOM handling, and last-event-id tracking
- WASM/browser runtime support — portable modules (decompress, digest_auth, proxy, timing) ungated for `wasm32`; WASI-P2 client support
- Vectored writes for compio and smol runtimes; eliminated boxed sleep allocations
- WASM integration tests running in headless Chrome CI

### Fixed
- `AioductBody` no longer requires `Sync` — relaxed unnecessary bound that prevented using non-`Sync` body types
- Adaptive h2c probe correctly detects h1-only servers — hyper's h2 handshake can "succeed" against an h1 server because it returns the sender before the server processes the preface; the probe now waits briefly and checks connection readiness

### Changed
- Dependency versions centralized in workspace root `Cargo.toml`; crate-level `Cargo.toml` files reference workspace versions
- Large modules split for maintainability; H2 config deduplicated

## [0.1.6] - 2026-04-30

### Added
- Request forwarding via `Client::forward(req)` — proxy/gateway builder that strips hop-by-hop headers, rewrites the URI to an upstream, streams the body without buffering, and bypasses all client middleware (redirects, cookies, cache, decompression). Supports path prefix stripping, host preservation, custom header injection/removal, and `on_request`/`on_response` hooks for escape-hatch mutations.
- WebSocket/HTTP upgrade forwarding — `ForwardBuilder` auto-detects upgrade requests (HTTP/1.1 `Connection: Upgrade` and HTTP/2 extended CONNECT via `Protocol` extension), preserves relevant headers through hop-by-hop stripping, and returns the response with an extractable `Upgraded` stream for bidirectional tunneling
- Re-export `hyper::ext::Protocol` for constructing H2 extended CONNECT requests without a direct hyper dependency

## [0.1.5] - 2026-04-30

### Fixed
- `Client::build()` no longer requires a runtime context — the pool reaper task is now spawned lazily on first connection checkin, allowing client construction in synchronous code outside a Tokio/smol/compio runtime

## [0.1.4] - 2026-04-28

### Added
- Pluggable cache store via `CacheStore` trait — implement custom backends (moka, foyer, Redis, etc.) and pass to `HttpCache::with_store()`
- `InMemoryCacheStore` extracted as the default `CacheStore` implementation
- `CacheEntry` made public for custom store implementations
- New public exports: `CacheStore`, `InMemoryCacheStore`, `CacheEntry`
- Per-request timing breakdown via `Response::timings()` — exposes DNS resolution, TCP connect, TLS handshake, transfer (TTFB), and total durations as `RequestTimings`
- Pool-hit requests report transfer and total only; skipped phases are `None`
- Integration tests for HTTP and HTTPS timing verification
- Explicit rustls crypto provider selection: `rustls-ring` and `rustls-aws-lc-rs` feature flags replace the implicit provider model; AWS-LC now supported for internally built TLS configurations and HTTP/3 (quinn) transport

### Fixed
- Redirect resolution replaced hand-rolled logic with `url::Url::join` — correctly handles relative paths, parent traversals (`../`), query-only redirects (`?q=`), and protocol-relative URLs (`//host/path`)
- Digest auth 401-retry path no longer loses the original request body on retry
- HTTP cache stores responses after decompression and header stripping so cache hits return the same content callers receive
- Cookie `host_only` flag tracked per RFC 6265 §5.3 — cookies set without an explicit `Domain` attribute no longer leak to subdomains
- JSON `Content-Type` preserved when callers set a custom content type
- Hickory DNS resolver falls back to default config when system DNS loading fails; parallel IPv4/IPv6 lookups enabled
- HTTP/3 connector tries all resolved addresses before failing, and binds default QUIC endpoints to IPv6 unspecified with IPv4 fallback
- HTTP/3 graceful `STOP_SENDING` (`H3_NO_ERROR`) no longer surfaces as a request error
- Tracing events record request host instead of full URIs — avoids leaking query strings, redirect targets, and credentials

## [0.1.3] - 2026-04-24

### Fixed
- Fixed TLS 1.3 handshake hang: flush client Finished message immediately after handshake loop completes, preventing HTTPS requests from stalling until timeout
- Moved `tokio-rustls` and `hyper-util` dev-dependencies to workspace, enforcing consistent dependency management

### Added
- HTTPS integration tests covering H2 over TLS, HTTP/1.1 over TLS, no-ALPN server, and `danger_accept_invalid_certs` paths

## [0.1.2] - 2026-04-20

### Fixed
- Fixed docs.rs build failure by gating `compile_error!` with `not(doc)` so rustdoc succeeds without a runtime feature
- Added `package.metadata.docs.rs` with `all-features = true` to expose the full API surface on docs.rs

## [0.1.1] - 2026-04-20

### Added
- HSTS (HTTP Strict Transport Security) auto-upgrade with `HstsStore` (RFC 6797)
- SameSite cookie attribute (Strict/Lax/None) and cookie prefix validation (__Host-, __Secure-) per RFC 6265bis
- `immutable` Cache-Control directive — skip revalidation for immutable resources (RFC 8246)
- `stale-while-revalidate` and `stale-if-error` Cache-Control extensions (RFC 5861)
- `stale-if-error` client fallback — serves stale cached responses when the origin returns 5xx or is unreachable, within the grace window
- Retry-After header parsing (seconds and HTTP-date formats) integrated into retry loop (RFC 9110)
- 429 Too Many Requests now triggers retry (alongside 5xx)
- Link header parsing with `Response::links()` (RFC 8288)
- RFC 9457 Problem Details response helper with `Response::problem_details()` (requires `json` feature)
- TCP Fast Open support on Linux via `ClientBuilder::tcp_fast_open()` (RFC 7413)
- Forwarded header builder and parser (RFC 7239)

### Changed
- Test suite expanded from 485 to 793 tests (95% line coverage)

### Fixed
- Resolved all clippy warnings under `--all-features --all-targets`
- Fixed env-var race conditions in netrc tests via serialization mutex

## [0.1.0] - 2026-04-19

### Added
- Async-native HTTP client built on hyper 1.x
- Runtime-agnostic design: tokio, smol, and compio support via feature flags
- HTTPS via rustls (no native-tls dependency)
- Connection pooling with LIFO ordering, readiness checks, and background reaper
- HTTP/2 multiplexing and connection tuning (`Http2Config`)
- HTTP/3 (QUIC) support via `http3` feature flag
- Automatic redirect following (301/302/303/307/308) with sensitive header stripping
- Request/response middleware layer (`Middleware` trait)
- Cookie jar for automatic cookie management
- Automatic response decompression (gzip, brotli, zstd, deflate)
- Server-Sent Events (SSE) streaming
- Multipart/form-data uploads
- Streaming request and response bodies
- Parallel chunk downloads with range requests
- JSON request/response support via `json` feature
- Retry with exponential backoff and jitter
- HTTP and SOCKS5 proxy support (including environment variable detection)
- Per-request and client-wide timeouts (connect + total)
- TCP keepalive and local address binding
- Custom DNS resolver support
- Bearer and Basic authentication helpers
- Happy Eyeballs (RFC 6555) connection racing — interleaves IPv6/IPv4 addresses with 250ms stagger
- HTTP Digest authentication — automatic 401 retry with MD5 challenge-response (RFC 7616)
- Bandwidth limiter — token-bucket byte-rate throttle for download speed limiting
- `.netrc` support — `Netrc` parser and `NetrcMiddleware` for automatic credential injection
- `aioduct-aria` — aria2-inspired parallel download CLI with segmented range requests
- `aioduct-curl` — curl-inspired HTTP CLI with familiar flags (-X, -d, -H, -o, -L, -u, etc.)
- Comprehensive test suite (485 tests)
- Criterion benchmarks comparing against reqwest
- mdbook documentation
