# Feature Flags

aioduct uses feature flags to control runtime, TLS, and serialization dependencies. The default feature set is **empty** — you must enable at least one runtime.

## Available Features

| Feature  | Dependencies                      | Stability    | Description                          |
|----------|-----------------------------------|--------------|--------------------------------------|
| `tokio`  | tokio                             | Stable       | Tokio async runtime                  |
| `smol`   | smol, async-io, futures-io        | Stable       | Smol async runtime                   |
| `compio` | compio-runtime, async-io          | Experimental | Compio runtime (io_uring / IOCP)     |
| `wasm`   | wasm-bindgen, web-sys, js-sys     | Experimental | Compatible browser/worker Fetch runtime |
| `wasi-p2` | wasi                             | Experimental | WASI Preview 2 guest HTTP client     |
| `wasmtime` | wasmtime, wasmtime-wasi, wasmtime-wasi-http | Experimental | Wasmtime host-side WASI HTTP adapter |
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
| `blocking`| selected native runtime          | Stable       | Synchronous wrapper for Tokio, smol, or compio clients |
| `hickory-dns` | hickory-resolver, tokio      | Stable       | DNS resolution via hickory           |
| `doh`   | hickory-resolver (https)            | Stable       | DNS-over-HTTPS (implies `hickory-dns`) |
| `dot`   | hickory-resolver (tls)              | Stable       | DNS-over-TLS (implies `hickory-dns`)   |
| `tower`  | tower-service, tower-layer        | Stable       | Tower Service/Layer integration      |
| `tracing`| tracing                           | Stable       | Tracing spans for HTTP requests      |
| `otel`   | opentelemetry, opentelemetry-http | Stable       | OpenTelemetry middleware             |
| `precise-timing` | none                       | Stable       | Use `std::time::Instant` instead of the default coarse clock for sub-millisecond observer and timeout measurements |
| `http3`  | [h3](https://crates.io/crates/h3), quinn | Experimental | HTTP/3 transport; currently requires Tokio, `rustls`, and one rustls provider |

## TLS Provider Features

Use `rustls` for the HTTPS backend and choose exactly one rustls crypto provider: `rustls-ring` or `rustls-aws-lc-rs`. The backend and provider flags are separate so future TLS backends, such as a reserved `native-tls`/OpenSSL backend, can compose with higher-level HTTP features without changing the rustls provider model. `rustls-native-roots` is provider-neutral: it enables the rustls backend and composes with either provider.

## Compile Error Without Runtime

If no runtime feature is selected, aioduct emits a compile error:

```text
error: aioduct: enable at least one runtime feature: tokio, smol, compio, wasm, or wasi-p2
```

## Common Feature Combinations

```toml
# HTTP only, tokio runtime
aioduct = { version = "0.2.5", features = ["tokio"] }

# HTTPS + JSON, tokio runtime
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "json"] }

# HTTPS with AWS-LC, tokio runtime
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-aws-lc-rs"] }

# HTTPS with AWS-LC and OS native roots
aioduct = { version = "0.2.5", features = ["tokio", "rustls-native-roots", "rustls-aws-lc-rs"] }

# HTTP only, smol runtime
aioduct = { version = "0.2.5", features = ["smol"] }

# HTTPS, smol runtime
aioduct = { version = "0.2.5", features = ["smol", "rustls", "rustls-ring"] }

# HTTP only, compio runtime (experimental)
aioduct = { version = "0.2.5", features = ["compio"] }

# HTTPS + JSON + compression, tokio runtime
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "json", "gzip", "brotli", "zstd", "deflate"] }

# Blocking client (select one native runtime; Tokio shown)
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "blocking"] }

# With tracing and OpenTelemetry
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "tracing", "otel"] }

# With tower integration
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "tower"] }

# Hickory DNS resolver
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "hickory-dns"] }

# DNS-over-HTTPS
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "doh"] }

# DNS-over-TLS
aioduct = { version = "0.2.5", features = ["tokio", "rustls", "rustls-ring", "dot"] }

# HTTP/3 with ring
aioduct = { version = "0.2.5", features = ["tokio", "http3", "rustls", "rustls-ring"] }

# HTTP/3 with AWS-LC
aioduct = { version = "0.2.5", features = ["tokio", "http3", "rustls", "rustls-aws-lc-rs"] }
```

## Wasmtime Host Adapter

Wasmtime host integration is first-party under `aioduct::wasmtime`. Enable the
`wasmtime` feature together with the host runtime and TLS provider:

```toml
# Wasmtime host adapter with tokio + rustls/ring
aioduct = { version = "0.2.5", features = ["wasmtime", "tokio", "rustls", "rustls-ring"] }

# Wasmtime host adapter with smol + rustls/ring
aioduct = { version = "0.2.5", features = ["wasmtime", "smol", "rustls", "rustls-ring"] }

# Wasmtime host adapter with compio + rustls/ring
aioduct = { version = "0.2.5", features = ["wasmtime", "compio", "rustls", "rustls-ring"] }
```

`aioduct::wasmtime` accepts native `RuntimePoll` transports such as
`TokioClient` and `SmolClient`. It also provides `CompioHostTransport`, which
owns a local-runtime worker bridge for `CompioClient` builders. It does not
enable browser `wasm`, because browser Fetch has no Wasmtime host hook. See
`examples/wasmtime-host` for runnable host examples for each native forwarding
transport.

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
