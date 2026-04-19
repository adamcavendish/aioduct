# Benchmarks

aioduct includes criterion benchmarks comparing performance against reqwest. Both clients are tested against a local hyper HTTP/1.1 server.

## Running Benchmarks

```bash
cargo bench --features tokio,rustls,json,gzip,brotli,zstd,deflate --bench comparison
```

Results are written to `target/criterion/` with HTML reports.

## Benchmark Scenarios

| Benchmark        | Description                                       |
|------------------|---------------------------------------------------|
| `single_get`     | Simple GET request, read body as bytes             |
| `single_get_text`| GET request, read body as UTF-8 string             |
| `json_parse`     | GET request, deserialize JSON response             |
| `concurrent_10`  | 10 concurrent GET requests via `tokio::spawn`      |
| `post_4k_body`   | POST with a 4 KB string body, read response bytes  |

All benchmarks reuse a single client and connection pool across iterations, measuring steady-state performance with warm connections.

## Notes

- Benchmarks use a local loopback server, so network latency is negligible — this isolates HTTP client overhead.
- reqwest is configured with default settings (native-tls, no custom options).
- aioduct is configured with default settings (no TLS for plain HTTP benchmarks).
- For real-world workloads, TLS handshake cost and network latency typically dominate; the differences shown here are in the protocol/client overhead.
