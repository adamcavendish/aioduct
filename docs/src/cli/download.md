# aioduct download

An aria2-style parallel downloader that splits files into segments and fetches them concurrently using HTTP Range requests.

## Basic Usage

```sh
# Download with auto-detected filename, 8 parallel segments
aioduct download https://releases.example.com/archive-2.1.tar.gz

# Custom output directory and filename
aioduct download -d ./downloads -o my-archive.tar.gz \
  https://releases.example.com/archive-2.1.tar.gz

# Multiple URIs
aioduct download \
  https://mirror1.example.com/file1.iso \
  https://mirror2.example.com/file2.iso

# Download from a URI list file
aioduct download -i urls.txt -d ./batch-output
```

## Parallelism

```sh
# 16 parallel segments per file
aioduct download -s 16 https://cdn.example.com/large-file.bin

# Limit connections per server
aioduct download -x 4 https://cdn.example.com/large-file.bin

# 3 concurrent downloads from a list
aioduct download -j 3 -i urls.txt

# Set minimum segment size (skip splitting small files)
aioduct download -k 10M https://cdn.example.com/large-file.bin
```

The downloader probes the server for `Accept-Ranges` support. If the server does not support range requests, it falls back to a single-connection download.

## Resume & Integrity

Re-running the same command automatically resumes interrupted downloads. Completed segments are skipped.

```sh
# Resume an interrupted download (just re-run)
aioduct download https://slow-server.example.com/10gb-backup.tar.zst

# Disable resume (re-download from scratch)
aioduct download --no-resume https://example.com/file.bin

# Verify checksum after download
aioduct download --checksum sha-256=e3b0c44298fc1c149afbf4c8996fb924... \
  https://releases.example.com/critical-binary
```

## WebDAV Recursive

Download entire directory trees from WebDAV servers. The URL must end with `/` to trigger directory listing.

```sh
# Recursive download of a WebDAV directory
aioduct download -r https://webdav.example.com/shared/project-assets/

# Limit recursion depth
aioduct download -r --max-depth 2 https://webdav.example.com/docs/
```

## Speed Limiting

```sh
# Global speed cap across all concurrent downloads
aioduct download --max-overall-download-limit 10M -j 5 -i urls.txt

# Per-download speed cap
aioduct download --max-download-limit 2M https://cdn.example.com/file.bin
```

## Dry Run

Probe URIs without downloading. Reports file size, range support, and output path.

```sh
aioduct download --dry-run https://cdn.example.com/huge-dataset.parquet
```

## Progress Display

By default, the downloader shows a ratatui TUI with per-file progress bars, speed, and ETA.

```sh
# Plain newline-based progress (no TUI)
aioduct download --plain https://example.com/file.bin

# Suppress all output
aioduct download -q https://example.com/file.bin

# Debug logging to file
aioduct download --log download.log --log-level debug https://example.com/file.bin
```

## Flags Reference

| Flag | Short | Description |
|------|-------|-------------|
| `URI...` | | Download URIs (positional, repeatable) |
| `--input-file FILE` | `-i` | Read URIs from file (one per line) |
| `--dir PATH` | `-d` | Output directory (default: `.`) |
| `--out FILENAME` | `-o` | Output filename (single URI only) |
| `--split N` | `-s` | Parallel connections per download (default: 8) |
| `--max-connection-per-server N` | `-x` | Max connections per server (default: 8) |
| `--max-concurrent-downloads N` | `-j` | Max concurrent downloads (default: 5) |
| `--min-split-size SIZE` | `-k` | Minimum segment size (default: 1M) |
| `--piece-size SIZE` | | Override piece size (auto if unset) |
| `--no-resume` | | Disable automatic resume |
| `--file-allocation METHOD` | | `none`, `prealloc` (default), or `falloc` |
| `--auto-file-renaming` | | Rename output if file exists |
| `--allow-overwrite` | | Overwrite existing files |
| `--checksum TYPE=DIGEST` | | Verify integrity (e.g. `sha-256=abc...`) |
| `--timeout SECS` | `-t` | Per-request timeout (default: 60s) |
| `--connect-timeout SECS` | | Connection timeout (default: 30s) |
| `--max-tries N` | `-m` | Max retry attempts (default: 5) |
| `--retry-wait SECS` | | Seconds between retries (default: 1) |
| `--max-overall-download-limit SIZE` | | Global speed cap |
| `--max-download-limit SIZE` | | Per-download speed cap |
| `--header NAME:VALUE` | `-H` | Extra HTTP header (repeatable) |
| `--referer URL` | | Referer header |
| `--user-agent STRING` | `-U` | User-Agent string |
| `--http-user USER` | | HTTP Basic auth username |
| `--http-passwd PASS` | | HTTP Basic auth password |
| `--all-proxy URL` | | Proxy for all protocols |
| `--check-certificate-false` | | Disable TLS verification |
| `--recursive` | `-r` | Recursive WebDAV download |
| `--max-depth N` | | Max recursion depth (default: unlimited) |
| `--dry-run` | | Probe without downloading |
| `--quiet` | `-q` | Suppress all output |
| `--plain` | | Plain text progress (no TUI) |
| `--human-readable` | | Human-readable sizes (default: true) |
| `--log FILE` | `-l` | Write debug log to file |
| `--log-level LEVEL` | | `trace`, `debug`, `info`, `warn`, `error` |

## Examples

```sh
# Batch download with global speed limit and plain progress
aioduct download -j 3 --max-overall-download-limit 20M --plain -i urls.txt

# Mirror a WebDAV directory with auth
aioduct download -r \
  --http-user deploy --http-passwd s3cret \
  https://webdav.example.com/releases/

# CI artifact fetch — fast, limited bandwidth, verified
aioduct download -s 16 \
  --max-download-limit 50M \
  --checksum sha-256=a1b2c3d4... \
  -d ./artifacts \
  https://ci.example.com/builds/latest/output.tar.gz

# Probe multiple files before downloading
aioduct download --dry-run \
  https://cdn.example.com/dataset-part1.parquet \
  https://cdn.example.com/dataset-part2.parquet \
  https://cdn.example.com/dataset-part3.parquet

# Download through SOCKS5 proxy with custom user-agent
aioduct download --all-proxy socks5://127.0.0.1:1080 \
  -U 'aioduct-bot/1.0' \
  https://private.example.com/internal-build.deb
```
