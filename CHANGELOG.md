# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.4] - 2026-04-25

### Added
- Pluggable cache store via `CacheStore` trait — implement custom backends (moka, foyer, Redis, etc.) and pass to `HttpCache::with_store()`
- `InMemoryCacheStore` extracted as the default `CacheStore` implementation
- `CacheEntry` made public for custom store implementations
- New public exports: `CacheStore`, `InMemoryCacheStore`, `CacheEntry`
- Per-request timing breakdown via `Response::timings()` — exposes DNS resolution, TCP connect, TLS handshake, transfer (TTFB), and total durations as `RequestTimings`
- Pool-hit requests report transfer and total only; skipped phases are `None`
- Integration tests for HTTP and HTTPS timing verification

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
