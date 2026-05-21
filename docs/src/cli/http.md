# aioduct http

A curl-style HTTP client with familiar flags and a real-time verbose TUI for inspecting request lifecycles.

## Basic Usage

```sh
# GET request
aioduct http https://httpbin.org/get

# POST with JSON body
aioduct http -X POST -d '{"key":"value"}' \
  -H 'Content-Type: application/json' \
  https://httpbin.org/post

# Form POST
aioduct http -F 'user=adam' -F 'action=deploy' https://httpbin.org/post

# Upload binary file
aioduct http -X PUT --data-binary @./artifact.tar.gz \
  -H 'Content-Type: application/octet-stream' \
  https://storage.example.com/uploads/artifact.tar.gz

# HEAD request — show response headers only
aioduct http -I https://example.com
```

## Verbose Mode

The `-v` flag activates verbose output showing the full request lifecycle.

When stdout is a terminal, verbose mode launches a ratatui TUI with a phase timeline, request/response headers, and a status bar:

```
┌─────────────────────────────┬──────────────────────────────────┐
│  TIMELINE                   │  REQUEST HEADERS                 │
│                             │  > host: api.example.com         │
│  ● DNS  1.2.3.4    12.3ms  │  > content-type: application/json│
│  ● TCP             23.1ms  │  > authorization: Bearer ***     │
│  ● TLS             45.7ms  │                                  │
│  ● REQ              1.8ms  ├──────────────────────────────────┤
│  ● WAIT           187.2ms  │  BODY                            │
│  ● RESP 200       270.1ms  │                                  │
│                             │                                  │
├─────────────────────────────┴──────────────────────────────────┤
│  200 OK | HTTP/2 | 93.184.216.34 | 270ms                      │
└────────────────────────────────────────────────────────────────┘
```

Press `Tab` to toggle between request and response headers. Press `q` to quit.

When stdout is not a terminal (piped), verbose output falls back to colored stderr text:

```sh
# TUI mode (terminal)
aioduct http -v https://example.com

# Force plain text verbose (always stderr, even on a terminal)
aioduct http --verbose-plain https://example.com

# Pipe-friendly: body to stdout, verbose to stderr
aioduct http -v https://api.example.com/data | jq .
```

## Authentication

```sh
# HTTP Basic auth
aioduct http -u admin:secret https://httpbin.org/basic-auth/admin/secret

# Bearer token
aioduct http --oauth2-bearer eyJhbGciOi... https://api.example.com/protected
```

## Output Control

```sh
# Save response body to file
aioduct http -o page.html https://example.com

# Save using filename from URL
aioduct http -O https://releases.example.com/v2.1/archive.tar.gz

# Dump response headers to file
aioduct http -D headers.txt https://example.com

# Include response headers in stdout
aioduct http -i https://example.com

# Write-out format (status code for scripting)
aioduct http -s -o /dev/null -w '%{http_code}\n' https://example.com
```

## Redirects & Retries

```sh
# Follow redirects (up to 10 hops by default)
aioduct http -L https://httpbin.org/redirect/3

# Custom redirect limit
aioduct http -L --max-redirs 5 https://httpbin.org/redirect/3

# Retry on failure with exponential backoff
aioduct http --retry 5 --retry-max-time 120 https://flaky-service.example.com/health
```

## Transport

```sh
# Force HTTP/2 prior knowledge
aioduct http --http2 https://example.com

# Request compressed response
aioduct http --compressed https://cdn.example.com/large-payload.json

# Limit download speed
aioduct http --limit-rate 1M https://cdn.example.com/file.bin

# HTTP proxy
aioduct http -x http://proxy:8080 https://example.com

# SOCKS5 proxy
aioduct http -x socks5://127.0.0.1:1080 https://example.com

# Skip TLS verification
aioduct http -k https://self-signed.example.com

# Timeouts
aioduct http --connect-timeout 5 --max-time 30 https://slow-server.example.com
```

## Flags Reference

| Flag | Long | Description |
|------|------|-------------|
| `-X` | `--request` | HTTP method |
| `-d` | `--data` | Request body (implies POST); prefix `@` to read from file |
| | `--data-binary` | Binary body; prefix `@` to read from file |
| `-F` | `--form` | URL-encoded form field (repeatable) |
| `-H` | `--header` | Extra header (repeatable) |
| `-A` | `--user-agent` | User-Agent string |
| `-e` | `--referer` | Referer URL |
| `-u` | `--user` | Basic auth (`user:password`) |
| | `--oauth2-bearer` | Bearer token |
| `-L` | `--location` | Follow redirects |
| | `--max-redirs` | Max redirect hops (default: 10) |
| `-I` | `--head` | HEAD request, show headers only |
| `-i` | `--include` | Include response headers in output |
| `-v` | `--verbose` | Verbose mode (TUI on terminal, plain text otherwise) |
| | `--verbose-plain` | Force plain-text verbose to stderr |
| `-s` | `--silent` | Silent mode |
| `-S` | `--show-error` | Show errors in silent mode |
| `-o` | `--output` | Write body to file |
| `-O` | `--remote-name` | Save using filename from URL |
| `-D` | `--dump-header` | Dump response headers to file |
| `-w` | `--write-out` | Format string (`%{http_code}`, `%{response_code}`) |
| `-m` | `--max-time` | Total request timeout (seconds) |
| | `--connect-timeout` | Connection timeout (seconds) |
| | `--retry` | Retry count |
| | `--retry-max-time` | Max backoff between retries (default: 60s) |
| `-x` | `--proxy` | Proxy URL (HTTP or SOCKS5) |
| `-k` | `--insecure` | Skip TLS verification |
| | `--http2` | Force HTTP/2 prior knowledge |
| | `--limit-rate` | Max download speed (supports K/M/G) |
| | `--raw` | Disable decompression |
| | `--compressed` | Request compressed response |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Generic error |
| 3 | Invalid URL |
| 7 | Connection / I/O error |
| 22 | HTTP 4xx/5xx response |
| 23 | Output write error |
| 28 | Timeout |
| 60 | TLS error |

## Examples

```sh
# Query an LLM API with bearer auth and parse the JSON response
aioduct http -X POST \
  --oauth2-bearer sk-abc123 \
  -H 'Content-Type: application/json' \
  -d '{"model":"claude-3","prompt":"Hello"}' \
  https://api.anthropic.com/v1/messages | jq .content

# CI health check — exit non-zero on failure
aioduct http -s -o /dev/null -w '%{http_code}' \
  --max-time 5 --retry 3 \
  https://production.example.com/healthz

# Upload a release artifact through a corporate proxy
aioduct http -X PUT --data-binary @./build/release.tar.gz \
  -H 'Content-Type: application/gzip' \
  -x http://corporate-proxy:3128 \
  -u deploy:token \
  https://artifacts.example.com/releases/v2.1/release.tar.gz

# Inspect TLS and timing details with verbose plain output
aioduct http --verbose-plain --http2 https://example.com 2>&1 | grep -E 'TLS|RESP'

# Download with speed limit and save to specific file
aioduct http --limit-rate 500K -o large-file.bin \
  https://cdn.example.com/datasets/training-data.bin

# Silent mode with write-out for monitoring scripts
aioduct http -s -o /dev/null \
  -w 'status=%{http_code}\n' \
  --max-time 10 \
  https://api.example.com/status
```

