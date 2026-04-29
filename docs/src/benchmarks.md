# Benchmarks

aioduct includes [criterion](https://github.com/bheisler/criterion.rs) benchmarks comparing HTTP client overhead against popular alternatives.

| Crate | Version | Description |
|-------|---------|-------------|
| **aioduct** | 0.1.0 | This crate — hyper 1.x, no hyper-util, async-native |
| **reqwest** | 0.12 | The most popular Rust HTTP client, built on hyper + hyper-util + tower |
| **hyper-util** | 0.1 | hyper's official high-level client (`legacy::Client`), minimal wrapper |
| **isahc** | 1.8 | Built on libcurl via curl-sys, independent HTTP stack |

## Setup

All benchmarks hit a **local hyper server** on loopback (`127.0.0.1`), eliminating network latency to isolate pure client overhead. Each benchmark reuses a single client and connection pool across iterations, measuring steady-state performance with warm connections.

## Running

```bash
# All H1 benchmarks
cargo bench --manifest-path crates/aioduct-bench/Cargo.toml --bench h1

# All H2 benchmarks
cargo bench --manifest-path crates/aioduct-bench/Cargo.toml --bench h2

# JSON benchmarks
cargo bench --manifest-path crates/aioduct-bench/Cargo.toml --bench json

# Feature benchmarks (SSE, multipart, streaming, chunk download)
cargo bench --manifest-path crates/aioduct-bench/Cargo.toml --bench features

# Connection pool benchmarks
cargo bench --manifest-path crates/aioduct-bench/Cargo.toml --bench pooling
```

HTML reports are generated in `target/criterion/`.

## Results

Measured on Linux 5.15, Rust 1.85, release profile. Times are the **mean** of 30–100 samples (lower is better).

### HTTP/1.1 GET Request (bytes)

Simple GET, read entire response as `Bytes`.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **43.0 µs** | — |
| hyper-util | 44.8 µs | +4.2% |
| reqwest | 48.6 µs | +13.0% |
| isahc | 91.3 µs | +112.3% |

### HTTP/1.1 GET Request (text)

GET, read response as UTF-8 `String`.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **44.7 µs** | — |
| reqwest | 47.5 µs | +6.3% |

### JSON Deserialization

GET + deserialize a small JSON object (`{"message":"hello","count":42}`).

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **43.6 µs** | — |
| reqwest | 47.4 µs | +8.7% |

### POST with 4 KB Body

POST a 4 KB string, read response bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **53.3 µs** | — |
| reqwest | 59.8 µs | +12.2% |
| isahc | 76.2 µs | +43.0% |

### Large Body Download (64 KB, HTTP/1.1)

GET a 64 KB response, read as bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| hyper-util | **60.1 µs** | -4.0% |
| **aioduct** | 62.6 µs | — |
| reqwest | 64.5 µs | +3.0% |

### Large Body Download (1 MB, HTTP/1.1)

GET a 1 MB response, read as bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **465.8 µs** | — |
| reqwest | 481.4 µs | +3.3% |

### 10 Concurrent Requests (HTTP/1.1)

10 GET requests dispatched via `tokio::spawn`, all awaited.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **124.3 µs** | — |
| reqwest | 140.9 µs | +13.4% |

### 50 Concurrent Requests (HTTP/1.1)

50 GET requests dispatched via `tokio::spawn`, all awaited.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **361.5 µs** | — |
| reqwest | 425.0 µs | +17.6% |

### HTTP/2 GET Request

GET via h2c (HTTP/2 over cleartext).

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **61.5 µs** | — |
| hyper-util | 84.7 µs | +37.7% |

### HTTP/2 Download (64 KB)

GET a 64 KB response via h2c.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **105.1 µs** | — |
| hyper-util | 2,068 µs | +1868% |

*(hyper-util h2 uses default 64 KB window sizes, hitting flow-control bottlenecks on larger payloads. aioduct configures 2 MB stream / 4 MB connection windows.)*

### HTTP/2 Download (1 MB)

GET a 1 MB response via h2c (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **734.0 µs** |

### HTTP/2 10 Concurrent Requests

10 concurrent requests multiplexed over a single h2c connection (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **162.1 µs** |

### HTTP/2 POST with 4 KB Body

POST a 4 KB payload via h2c (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **87.7 µs** |

### Connection Pool Overhead

Comparison of pooled vs no-pool (fresh connection per request).

| Protocol | With Pool | No Pool | Speedup |
|----------|-----------|---------|---------|
| HTTP/1.1 | **44.8 µs** | 95.4 µs | 2.1× |
| HTTP/2   | **80.6 µs** | 191.4 µs | 2.4× |

### SSE: Consume 100 Events

Parse 100 Server-Sent Events from a single response (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **65.4 µs** |

### Multipart Upload (small)

Multipart form with two text fields.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **50.8 µs** | — |
| reqwest | 66.6 µs | +31.1% |

### Multipart Upload (1 MB file)

Multipart form with a 1 MB file part.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **846.3 µs** | — |
| reqwest | 944.9 µs | +11.7% |

### Streaming Upload (1 MB)

Stream a 1 MB body to an echo server.

| Client | Mean | vs aioduct |
|--------|------|------------|
| reqwest | **750.8 µs** | -2.6% |
| **aioduct** | 770.9 µs | — |

### Chunk Download (1 MB)

Parallel range-based download of a 1 MB file.

| Chunks | Mean |
|--------|------|
| 1 chunk | 2,239 µs |
| 4 chunks | 2,308 µs |
| 8 chunks | 2,297 µs |
| Single GET (baseline) | **362.5 µs** |

*(On loopback the overhead of multiple range requests exceeds the parallelism benefit. Chunk download shows gains on real networks with higher latency.)*

### Body Stream (64 KB)

Read a 64 KB response frame-by-frame vs collected as bytes (aioduct only).

| Method | Mean |
|--------|------|
| bytes collect | **56.0 µs** |
| frame by frame | 69.3 µs |

## Analysis

- **aioduct** is the fastest or tied for fastest in most benchmarks, sitting close to raw hyper-util while providing a much higher-level API (connection pooling, redirects, cookies, middleware, retry, etc.).
- **hyper-util** (`legacy::Client`) is close to aioduct in H1 but struggles in H2 due to default flow-control window sizes.
- **reqwest** is 3–31% slower than aioduct in most scenarios. The gap widens for concurrent workloads and multipart uploads.
- **isahc** is 43–112% slower due to the libcurl FFI boundary and curl's internal buffering.
- **Connection pooling** provides a consistent ~2× speedup over fresh connections for both H1 and H2.

## Caveats

- These benchmarks measure **loopback HTTP client overhead only**. In real-world usage, TLS handshakes and network latency dominate.
- reqwest uses native-tls by default (disabled here since we test plain HTTP).
- isahc uses libcurl which has its own connection pooling; the curl overhead is most visible on small payloads.
- The H2 comparison is not apples-to-apples: aioduct configures larger flow-control windows. With matching configuration hyper-util would be closer.
- Results vary by machine, OS, and Rust version. Run the benchmarks yourself for your environment.

## Benchmark Suites

| Suite | Bench File | Scenarios |
|-------|-----------|-----------|
| `h1` | `benches/h1.rs` | GET bytes/text, POST 4K, download 64K/1M, concurrent 10/50 |
| `h2` | `benches/h2.rs` | GET, download 64K/1M, concurrent 10, POST 4K |
| `json` | `benches/json.rs` | JSON deserialization (GET + serde) |
| `features` | `benches/features.rs` | SSE, multipart, upload 1M, chunk download, body stream |
| `pooling` | `benches/pooling.rs` | H1/H2 with-pool vs no-pool |
