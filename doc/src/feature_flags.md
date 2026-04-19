# Feature Flags

aioduct uses feature flags to control runtime, TLS, and serialization dependencies. The default feature set is **empty** — you must enable at least one runtime.

## Available Features

| Feature  | Dependencies                      | Stability    | Description                          |
|----------|-----------------------------------|--------------|--------------------------------------|
| `tokio`  | tokio                             | Stable       | Tokio async runtime                  |
| `smol`   | smol, async-io, futures-io        | Stable       | Smol async runtime                   |
| `compio` | compio-runtime, async-io          | Experimental | Compio runtime (io_uring / IOCP)     |
| `rustls` | rustls, webpki-roots, rustls-pemfile | Stable    | TLS via rustls (required for HTTPS)  |
| `json`   | serde, serde_json                 | Stable       | JSON request/response helpers        |
| `gzip`   | flate2                            | Stable       | Gzip response decompression          |
| `deflate`| flate2                            | Stable       | Deflate response decompression       |
| `brotli` | brotli                            | Stable       | Brotli response decompression        |
| `zstd`   | zstd                              | Stable       | Zstd response decompression          |
| `http3`  | h3, h3-quinn, quinn (+ rustls)    | Experimental | HTTP/3 transport                     |

## Compile Error Without Runtime

If no runtime feature is selected, aioduct emits a compile error:

```text
error: aioduct: enable at least one runtime feature: tokio, smol, or compio
```

## Common Feature Combinations

```toml
# HTTP only, tokio runtime
aioduct = { version = "0.1", features = ["tokio"] }

# HTTPS + JSON, tokio runtime
aioduct = { version = "0.1", features = ["tokio", "rustls", "json"] }

# HTTP only, smol runtime
aioduct = { version = "0.1", features = ["smol"] }

# HTTPS, smol runtime
aioduct = { version = "0.1", features = ["smol", "rustls"] }

# HTTP only, compio runtime (experimental)
aioduct = { version = "0.1", features = ["compio"] }

# HTTPS + JSON + compression, tokio runtime
aioduct = { version = "0.1", features = ["tokio", "rustls", "json", "gzip", "brotli", "zstd", "deflate"] }
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
