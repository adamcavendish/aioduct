# Feature Flags

aioduct uses feature flags to control runtime, TLS, and serialization dependencies. The default feature set is **empty** — you must enable at least one runtime.

## Available Features

| Feature  | Dependencies                      | Stability    | Description                          |
|----------|-----------------------------------|--------------|--------------------------------------|
| `tokio`  | tokio                             | Stable       | Tokio async runtime                  |
| `smol`   | smol, async-io, futures-io        | Stable       | Smol async runtime                   |
| `compio` | compio-runtime, async-io          | Experimental | Compio runtime (io_uring / IOCP)     |
| `wasm`   | wasm-bindgen, web-sys, js-sys     | Experimental | Browser/WASM runtime                 |
| `rustls` | rustls, webpki-roots, rustls-pemfile | Stable | TLS backend via rustls; requires exactly one rustls provider |
| `rustls-ring` | rustls ring provider        | Stable       | Ring crypto provider for rustls      |
| `rustls-aws-lc-rs` | rustls AWS-LC provider | Stable       | AWS-LC crypto provider for rustls    |
| `rustls-native-roots` | rustls-native-certs | Stable | Use OS certificate store with either rustls provider |
| `json`   | serde, serde_json, serde_urlencoded | Stable    | JSON request/response helpers        |
| `charset`| encoding_rs, mime                 | Stable       | Charset decoding for response text   |
| `gzip`   | flate2                            | Stable       | Gzip response decompression          |
| `deflate`| flate2                            | Stable       | Deflate response decompression       |
| `brotli` | brotli                            | Stable       | Brotli response decompression        |
| `zstd`   | zstd                              | Stable       | Zstd response decompression          |
| `blocking`| tokio                            | Stable       | Synchronous blocking client wrapper  |
| `hickory-dns` | hickory-resolver, tokio      | Stable       | DNS resolution via hickory           |
| `tower`  | tower-service, tower-layer        | Stable       | Tower Service/Layer integration      |
| `tracing`| tracing                           | Stable       | Tracing spans for HTTP requests      |
| `otel`   | opentelemetry, opentelemetry-http | Stable       | OpenTelemetry middleware             |
| `http3`  | h3, h3-quinn, quinn | Experimental | HTTP/3 transport; currently requires `rustls` plus one rustls provider |

## TLS Provider Features

Use `rustls` for the HTTPS backend and choose exactly one rustls crypto provider: `rustls-ring` or `rustls-aws-lc-rs`. The backend and provider flags are separate so future TLS backends, such as a reserved `native-tls`/OpenSSL backend, can compose with higher-level HTTP features without changing the rustls provider model. `rustls-native-roots` is provider-neutral: it enables the rustls backend and composes with either provider.

## Compile Error Without Runtime

If no runtime feature is selected, aioduct emits a compile error:

```text
error: aioduct: enable at least one runtime feature: tokio, smol, compio, or wasm
```

## Common Feature Combinations

```toml
# HTTP only, tokio runtime
aioduct = { version = "0.1", features = ["tokio"] }

# HTTPS + JSON, tokio runtime
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring", "json"] }

# HTTPS with AWS-LC, tokio runtime
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-aws-lc-rs"] }

# HTTPS with AWS-LC and OS native roots
aioduct = { version = "0.1", features = ["tokio", "rustls-native-roots", "rustls-aws-lc-rs"] }

# HTTP only, smol runtime
aioduct = { version = "0.1", features = ["smol"] }

# HTTPS, smol runtime
aioduct = { version = "0.1", features = ["smol", "rustls", "rustls-ring"] }

# HTTP only, compio runtime (experimental)
aioduct = { version = "0.1", features = ["compio"] }

# HTTPS + JSON + compression, tokio runtime
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring", "json", "gzip", "brotli", "zstd", "deflate"] }

# Blocking client (synchronous)
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring", "blocking"] }

# With tracing and OpenTelemetry
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring", "tracing", "otel"] }

# With tower integration
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring", "tower"] }

# Hickory DNS resolver
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring", "hickory-dns"] }

# HTTP/3 with ring
aioduct = { version = "0.1", features = ["tokio", "http3", "rustls", "rustls-ring"] }

# HTTP/3 with AWS-LC
aioduct = { version = "0.1", features = ["tokio", "http3", "rustls", "rustls-aws-lc-rs"] }
```

## Core Dependencies (Always Included)

These are pulled in regardless of feature flags:

- `hyper` 1.x — HTTP/1.1 and HTTP/2 protocol engine
- `http` — Standard HTTP types (Method, StatusCode, HeaderMap, etc.)
- `http-body-util` — Body combinators for hyper
- `bytes` — Zero-copy byte buffers
- `pin-project-lite` — Safe pin projections
- `thiserror` — Error derive macros
- `base64` — Base64 encoding for basic auth
- `percent-encoding` — URL percent-encoding for query params and forms
