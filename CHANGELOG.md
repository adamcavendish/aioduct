# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
- Comprehensive test suite (78 integration tests)
- Criterion benchmarks comparing against reqwest
- mdbook documentation
