# Benchmarks

aioduct includes [criterion](https://github.com/bheisler/criterion.rs) benchmarks comparing HTTP client overhead against three popular alternatives:

| Crate | Version | Description |
|-------|---------|-------------|
| **aioduct** | 0.1.0 | This crate — hyper 1.x, no hyper-util, async-native |
| **reqwest** | 0.12 | The most popular Rust HTTP client, built on hyper + hyper-util + tower |
| **hyper-util** | 0.1 | hyper's official high-level client (`legacy::Client`), minimal wrapper |
| **isahc** | 1.8 | Built on libcurl via curl-sys, independent HTTP stack |

## Setup

All benchmarks hit a **local hyper HTTP/1.1 server** on loopback (`127.0.0.1`), eliminating network latency to isolate pure client overhead. Each benchmark reuses a single client and connection pool across iterations, measuring steady-state performance with warm connections.

## Running

```bash
cargo bench --features tokio,rustls,json,gzip,brotli,zstd,deflate --bench comparison
```

HTML reports are generated in `target/criterion/`.

## Results

Measured on Linux 5.15, Rust 1.85, release profile. Times are the **mean** of 50-100 samples (lower is better).

### Single GET Request (bytes)

Simple GET, read entire response as `Bytes`.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **45.8 µs** | — |
| hyper-util | 47.9 µs | +4.6% |
| reqwest | 49.0 µs | +7.0% |
| isahc | 91.9 µs | +100.7% |

### Single GET Request (text)

GET, read response as UTF-8 `String`.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **45.7 µs** | — |
| hyper-util | 48.0 µs | +5.0% |
| reqwest | 49.3 µs | +7.9% |
| isahc | 93.0 µs | +103.5% |

### JSON Deserialization

GET + deserialize a small JSON object (`{"message":"hello","count":42}`).

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **46.7 µs** | — |
| hyper-util | 47.1 µs | +0.9% |
| reqwest | 49.7 µs | +6.4% |
| isahc | 83.2 µs | +78.2% |

### 10 Concurrent Requests

10 GET requests dispatched via `tokio::spawn`, all awaited.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **139.0 µs** | — |
| reqwest | 156.5 µs | +12.6% |

*(hyper-util and isahc omitted — their `Client` types are not easily `Send` across spawn boundaries without additional wrappers.)*

### POST with 4 KB Body

POST a 4 KB string, read response bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **50.2 µs** | — |
| reqwest | 56.0 µs | +11.6% |
| isahc | 92.1 µs | +83.5% |

### Large Body Download (64 KB)

GET a 64 KB response, read as bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **56.9 µs** | — |
| hyper-util | 62.2 µs | +9.3% |
| reqwest | 61.5 µs | +8.1% |
| isahc | 128.6 µs | +125.9% |

## Analysis

- **aioduct** is the fastest or tied for fastest in every benchmark, sitting close to raw hyper-util while providing a much higher-level API (connection pooling, redirects, cookies, middleware, etc.).
- **hyper-util** (`legacy::Client`) is consistently close to aioduct. The small gap is aioduct's additional features (redirect following, header management, middleware) which add negligible overhead.
- **reqwest** is 5-12% slower than aioduct. The extra overhead comes from its tower middleware stack, hyper-util indirection, and additional response processing layers.
- **isahc** is roughly 2x slower due to the libcurl FFI boundary and curl's internal buffering model.

## Caveats

- These benchmarks measure **loopback HTTP client overhead only**. In real-world usage, TLS handshakes and network latency dominate — the differences shown here are in the protocol/client layer.
- reqwest uses native-tls by default (disabled for these benchmarks since we test plain HTTP).
- isahc uses libcurl which has its own connection pooling; the curl overhead is most visible on small payloads.
- Results vary by machine, OS, and Rust version. Run the benchmarks yourself for your environment.

## Benchmark Scenarios

| Benchmark | Description |
|-----------|-------------|
| `single_get` | GET → bytes |
| `single_get_text` | GET → UTF-8 string |
| `json_parse` | GET → deserialized JSON |
| `concurrent_10` | 10 concurrent GET → bytes (aioduct, reqwest only) |
| `post_4k_body` | POST 4 KB → bytes |
| `large_body_64k` | GET 64 KB response → bytes |
