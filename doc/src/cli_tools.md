# CLI Tools

The aioduct workspace includes two command-line tools that serve as both useful utilities and real-world integration examples of the library.

## aioduct-aria

An aria2-inspired parallel download tool. It probes the server for `Accept-Ranges` support, splits the file into segments, and downloads them concurrently.

### Usage

```sh
# Basic download
aioduct-aria https://releases.example.com/archive.tar.gz

# 16 parallel segments, save to specific file
aioduct-aria -s 16 -o local.tar.gz https://releases.example.com/archive.tar.gz

# Resume an interrupted download
aioduct-aria -c https://releases.example.com/archive.tar.gz

# Limit download speed to 5 MB/s
aioduct-aria --max-download-speed 5M https://releases.example.com/archive.tar.gz

# Through a proxy with custom headers
aioduct-aria --proxy http://proxy:8080 -H "Authorization: Bearer tok" https://example.com/file
```

### Features

- Segmented parallel downloads using HTTP Range requests
- Automatic filename detection from `Content-Disposition` or URL
- Resume support (`-c`) — skips already-downloaded segments
- Progress bar with speed and ETA
- Bandwidth limiting
- Proxy support (HTTP and SOCKS5)
- Custom headers, basic auth, bearer auth
- Auto-rename to avoid overwriting existing files

## aioduct-curl

A curl-inspired HTTP tool with familiar flags. Covers the most commonly used curl options.

### Usage

```sh
# GET request
aioduct-curl https://httpbin.org/get

# POST with JSON
aioduct-curl -X POST -d '{"key":"val"}' -H 'Content-Type: application/json' https://httpbin.org/post

# HEAD request (show headers only)
aioduct-curl -I https://example.com

# Follow redirects with basic auth
aioduct-curl -L -u user:pass https://httpbin.org/basic-auth/user/pass

# Save response to file
aioduct-curl -o page.html https://example.com

# Verbose output
aioduct-curl -v https://example.com

# Write-out format (status code)
aioduct-curl -w '%{http_code}\n' -o /dev/null -s https://example.com
```

### Supported Flags

| Flag | Long | Description |
|------|------|-------------|
| `-X` | `--request` | HTTP method |
| `-d` | `--data` | Request body (sets POST) |
| | `--data-binary` | Binary request body (supports `@file`) |
| `-F` | `--form` | Form field (repeatable) |
| `-H` | `--header` | Extra header (repeatable) |
| `-A` | `--user-agent` | User-Agent string |
| `-e` | `--referer` | Referer URL |
| `-u` | `--user` | Basic auth (`user:password`) |
| | `--oauth2-bearer` | Bearer token |
| `-L` | `--location` | Follow redirects |
| | `--max-redirs` | Max redirect hops (default: 10) |
| `-I` | `--head` | HEAD request, show headers only |
| `-i` | `--include` | Include response headers in output |
| `-v` | `--verbose` | Show request and response headers |
| `-s` | `--silent` | Silent mode |
| `-S` | `--show-error` | Show errors in silent mode |
| `-o` | `--output` | Write to file |
| `-O` | `--remote-name` | Save using filename from URL |
| `-D` | `--dump-header` | Dump headers to file |
| `-w` | `--write-out` | Format string (`%{http_code}`) |
| `-m` | `--max-time` | Total request timeout (seconds) |
| | `--connect-timeout` | Connection timeout (seconds) |
| | `--retry` | Retry count |
| `-x` | `--proxy` | Proxy URL |
| `-k` | `--insecure` | Skip TLS verification |
| | `--http2` | Force HTTP/2 |
| | `--limit-rate` | Max download speed (supports K/M/G) |
| | `--raw` | Disable decompression |
| | `--compressed` | Request compressed response |

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Generic error |
| 3 | Invalid URL |
| 7 | Connection failed |
| 22 | HTTP 4xx/5xx response |
| 23 | Write error |
| 28 | Timeout |
| 60 | TLS error |
