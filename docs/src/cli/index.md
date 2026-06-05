# CLI Tools

The `aioduct` binary is a unified HTTP toolkit providing two subcommands: a curl-style HTTP client and an aria2-style parallel downloader. Both share the same connection pool, TLS stack, and HTTP/2 implementation from the aioduct library.

## Installation

**Shell installer (no Rust toolchain needed):**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/adamcavendish/aioduct/releases/download/0.2.0-alpha.6/aioduct-cli-installer.sh | sh
```

**Nightly build (latest master):**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/adamcavendish/aioduct/releases/download/nightly/aioduct-cli-installer.sh | sh
```

**Build from source:**

```sh
cargo install --git https://github.com/adamcavendish/aioduct aioduct-cli
```

## Subcommands

| Command | Description |
|---------|-------------|
| `aioduct http` | HTTP request tool with verbose TUI, redirects, retries, proxy |
| `aioduct download` | Parallel segmented downloader with resume, WebDAV recursion |
| `aioduct version` | Print version and exit |

## Size Suffixes

Both subcommands accept human-readable size values for bandwidth and segment options:

| Suffix | Multiplier |
|--------|-----------|
| (none) | bytes |
| `K` / `k` | x 1024 |
| `M` / `m` | x 1024² |
| `G` / `g` | x 1024³ |

Decimal fractions are supported: `1.5M` = 1,572,864 bytes.

## Comparison

**vs curl:** `aioduct http` covers the most commonly used curl flags with the same short options (`-X`, `-d`, `-H`, `-o`, `-L`, `-v`, etc.). The verbose mode (`-v`) provides a real-time ratatui TUI showing DNS/TCP/TLS/TTFB phases as a timeline, rather than plain text headers.

**vs aria2c:** `aioduct download` implements segmented parallel downloads with automatic resume, similar to `aria2c`. It adds WebDAV recursive directory downloads, checksum verification, and a TUI progress display. The flag names follow aria2 conventions (`--split`, `--max-concurrent-downloads`, `--min-split-size`).

**Shared advantages:** Both subcommands benefit from the aioduct library's HTTP/2 multiplexing, connection pooling, bandwidth limiting, and proxy support (HTTP, HTTPS, SOCKS4/SOCKS4a, SOCKS5, SOCKS5h) without external dependencies like libcurl or OpenSSL.
