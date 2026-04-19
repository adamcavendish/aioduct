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
| **aioduct** | **42.7 µs** | — |
| hyper-util | 46.0 µs | +7.7% |
| reqwest | 47.8 µs | +11.9% |
| isahc | 71.8 µs | +68.1% |

### HTTP/1.1 GET Request (text)

GET, read response as UTF-8 `String`.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **35.4 µs** | — |
| reqwest | 51.2 µs | +44.6% |

### JSON Deserialization

GET + deserialize a small JSON object (`{"message":"hello","count":42}`).

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **45.4 µs** | — |
| reqwest | 49.4 µs | +8.8% |

### POST with 4 KB Body

POST a 4 KB string, read response bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **50.9 µs** | — |
| reqwest | 52.8 µs | +3.7% |
| isahc | 79.7 µs | +56.6% |

### Large Body Download (64 KB, HTTP/1.1)

GET a 64 KB response, read as bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| hyper-util | **48.9 µs** | -20.6% |
| reqwest | 50.5 µs | -18.0% |
| **aioduct** | 61.6 µs | — |

### Large Body Download (1 MB, HTTP/1.1)

GET a 1 MB response, read as bytes.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **335.3 µs** | — |
| reqwest | 408.5 µs | +21.8% |

### 10 Concurrent Requests (HTTP/1.1)

10 GET requests dispatched via `tokio::spawn`, all awaited.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **123.8 µs** | — |
| reqwest | 140.3 µs | +13.3% |

### 50 Concurrent Requests (HTTP/1.1)

50 GET requests dispatched via `tokio::spawn`, all awaited.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **383.2 µs** | — |
| reqwest | 476.8 µs | +24.4% |

### HTTP/2 GET Request

GET via h2c (HTTP/2 over cleartext).

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **74.6 µs** | — |
| hyper-util | 78.1 µs | +4.7% |

### HTTP/2 Download (64 KB)

GET a 64 KB response via h2c.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **102.1 µs** | — |
| hyper-util | 2,068 µs | +1926% |

*(hyper-util h2 uses default 64 KB window sizes, hitting flow-control bottlenecks on larger payloads. aioduct configures 2 MB stream / 4 MB connection windows.)*

### HTTP/2 Download (1 MB)

GET a 1 MB response via h2c (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **781.7 µs** |

### HTTP/2 10 Concurrent Requests

10 concurrent requests multiplexed over a single h2c connection (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **157.3 µs** |

### HTTP/2 POST with 4 KB Body

POST a 4 KB payload via h2c (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **100.0 µs** |

### Connection Pool Overhead

Comparison of pooled vs no-pool (fresh connection per request).

| Protocol | With Pool | No Pool | Speedup |
|----------|-----------|---------|---------|
| HTTP/1.1 | **38.8 µs** | 94.4 µs | 2.4× |
| HTTP/2   | **78.5 µs** | 167.4 µs | 2.1× |

### SSE: Consume 100 Events

Parse 100 Server-Sent Events from a single response (aioduct only).

| Client | Mean |
|--------|------|
| **aioduct** | **76.9 µs** |

### Multipart Upload (small)

Multipart form with two text fields.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **52.9 µs** | — |
| reqwest | 65.2 µs | +23.3% |

### Multipart Upload (1 MB file)

Multipart form with a 1 MB file part.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **870.5 µs** | — |
| reqwest | 874.2 µs | +0.4% |

### Streaming Upload (1 MB)

Stream a 1 MB body to an echo server.

| Client | Mean | vs aioduct |
|--------|------|------------|
| **aioduct** | **760.7 µs** | — |
| reqwest | 785.3 µs | +3.2% |

### Chunk Download (1 MB)

Parallel range-based download of a 1 MB file.

| Chunks | Mean |
|--------|------|
| 1 chunk | 2,267 µs |
| 4 chunks | 2,294 µs |
| 8 chunks | 2,316 µs |
| Single GET (baseline) | **531.6 µs** |

*(On loopback the overhead of multiple range requests exceeds the parallelism benefit. Chunk download shows gains on real networks with higher latency.)*

### Body Stream (64 KB)

Read a 64 KB response frame-by-frame vs collected as bytes (aioduct only).

| Method | Mean |
|--------|------|
| bytes collect | 70.6 µs |
| frame by frame | **63.7 µs** |

## Analysis

- **aioduct** is the fastest or tied for fastest in most benchmarks, sitting close to raw hyper-util while providing a much higher-level API (connection pooling, redirects, cookies, middleware, retry, etc.).
- **hyper-util** (`legacy::Client`) is close to aioduct in H1 but struggles in H2 64 KB downloads due to default window sizes.
- **reqwest** is 4–45% slower than aioduct depending on the scenario. The gap widens for text decoding and concurrent workloads.
- **isahc** is 57–68% slower due to the libcurl FFI boundary and curl's internal buffering.
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
