# Proxy Support

aioduct supports routing requests through HTTP, HTTPS, SOCKS4/SOCKS4a, SOCKS5, and SOCKS5h proxies. Both HTTP and HTTPS targets use a CONNECT tunnel through HTTP and HTTPS proxies. SOCKS proxies tunnel all traffic regardless of scheme at the TCP level.

## Proxy Schemes

| Scheme | Constructor | DNS Resolution | Description |
|--------|------------|----------------|-------------|
| `http://` | `ProxyConfig::http()` | N/A | HTTP CONNECT proxy |
| `https://` | `ProxyConfig::https()` | N/A | TLS-wrapped HTTP CONNECT proxy |
| `socks4://` | `ProxyConfig::socks4()` | Local | SOCKS4 proxy |
| `socks4a://` | `ProxyConfig::socks4()` | Remote | SOCKS4a proxy (domain sent to proxy) |
| `socks5://` | `ProxyConfig::socks5()` | Local | SOCKS5 proxy (client resolves hostnames, sends IP) |
| `socks5h://` | `ProxyConfig::socks5h()` | Remote | SOCKS5h proxy (proxy resolves hostnames, sends domain) |

The difference between `socks5://` and `socks5h://` matters when the proxy
is on a different network (e.g. a corporate SOCKS proxy that can resolve
internal hostnames the client cannot).

### Auto-Detection from URL

`ProxyConfig::detect_from_url()` detects the proxy scheme from a URL string and
returns the appropriate `ProxyConfig`:

```rust,no_run
use aioduct::ProxyConfig;

// Recognised schemes
let http  = ProxyConfig::detect_from_url("http://proxy:8080");    // ProxyScheme::Http
let https = ProxyConfig::detect_from_url("https://proxy:443");    // ProxyScheme::Https
let s5    = ProxyConfig::detect_from_url("socks5://proxy:1080");  // ProxyScheme::Socks5
let s5h   = ProxyConfig::detect_from_url("socks5h://proxy:1080"); // ProxyScheme::Socks5h
let s4    = ProxyConfig::detect_from_url("socks4a://proxy:1080"); // ProxyScheme::Socks4

// Bare hostname:port — defaults to http://
let bare  = ProxyConfig::detect_from_url("proxy:3128");           // ProxyScheme::Http
```

This is used internally by `ProxySettings::from_env()` for environment variable
parsing and by the CLI's `-x` / `--proxy` flag.

## Basic Usage

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};

// HTTP proxy
let client = TokioClient::builder()
    .proxy(ProxyConfig::http("http://proxy.example.com:8080").unwrap())
    .build()?;

// HTTPS proxy (TLS-wrapped connection to the proxy)
let client = TokioClient::builder()
    .proxy(ProxyConfig::https("https://proxy.example.com:443").unwrap())
    .build()?;

// SOCKS5 proxy (local DNS)
let client = TokioClient::builder()
    .proxy(ProxyConfig::socks5("socks5://socks-proxy.example.com:1080").unwrap())
    .build()?;

// SOCKS5h proxy (remote DNS)
let client = TokioClient::builder()
    .proxy(ProxyConfig::socks5h("socks5h://socks-proxy.example.com:1080").unwrap())
    .build()?;

// SOCKS4/SOCKS4a proxy
let client = TokioClient::builder()
    .proxy(ProxyConfig::socks4("socks4a://socks-proxy.example.com:1080").unwrap())
    .build()?;
```

## URI-Embedded Credentials

Proxy URLs can include credentials in the standard `user:pass@host` format.
Both the username and password are percent-decoded automatically.

```rust,no_run
use aioduct::ProxyConfig;

// Credentials embedded in the URL
let proxy = ProxyConfig::http("http://alice:s3cret@proxy.example.com:8080").unwrap();

// Percent-encoded characters are decoded (e.g. %40 → @, %3A → :)
let proxy = ProxyConfig::https(
    "https://user%40domain:p%3Assword@proxy.example.com:443"
).unwrap();

// basic_auth() still works and overrides any URI-embedded credentials
let proxy = ProxyConfig::http("http://ignored:ignored@proxy:8080")
    .unwrap()
    .basic_auth("real-user", "real-pass");
```

## System Proxy (Environment Variables)

Use `system_proxy()` to read proxy settings from environment variables:

```rust,no_run
use aioduct::TokioClient;

let client = TokioClient::builder()
    .system_proxy()
    .build()?;
```

This reads:
- `HTTP_PROXY` / `http_proxy` — proxy for HTTP requests
- `HTTPS_PROXY` / `https_proxy` — proxy for HTTPS requests
- `NO_PROXY` / `no_proxy` — comma-separated list of hosts to bypass

The uppercase variant takes precedence over the lowercase variant. The
following URL schemes are recognised: `http://`, `https://`, `socks4://`,
`socks4a://`, `socks5://`, and `socks5h://`.

System proxy support is environment-based on native runtimes and blocking
clients. Wasm/browser and wasi-p2 transports are host-managed; proxy discovery,
DNS, and bypass behavior come from the browser or WASI host rather than
aioduct's native proxy stack.

### Runtime Scope

| Runtime | Proxy configuration | Bypass matching | Notes |
|---------|---------------------|-----------------|-------|
| Tokio | Native stack | `NoProxy` | HTTP, HTTPS, SOCKS4, SOCKS4a, SOCKS5, and SOCKS5h |
| smol | Native stack | `NoProxy` | Same behavior as Tokio for send clients |
| compio | Native stack | `NoProxy` | Same behavior through local clients |
| blocking | Wrapped native client | Wrapped native client | Inherits the configured async client behavior |
| wasm | Browser-managed | Browser-managed | The browser decides proxy routing and bypass rules |
| wasi-p2 | Host-managed | Host-managed | The WASI host decides proxy routing and bypass rules |

### NO_PROXY Rules

The `NO_PROXY` value is a comma-separated list of patterns:

| Pattern | Matches |
|---------|---------|
| `example.com` | `example.com` and `*.example.com` |
| `.example.com` | `*.example.com` (subdomains only) |
| `*` | All hosts (disables proxy) |
| `127.0.0.1` | Exact IP match |
| `10.0.0.0/8` | IPv4 CIDR match |
| `2001:db8::/32` | IPv6 CIDR match |
| `example.com:8080` | Hostname only when the request port is 8080 |
| `[2001:db8::1]:443` | IPv6 literal only when the request port is 443 |

Host matching is case-insensitive. A bare hostname rule matches that hostname
and its subdomains; a leading-dot rule matches subdomains only.

## Advanced: Separate HTTP/HTTPS Proxies

Use `ProxySettings` for fine-grained control:

```rust,no_run
use aioduct::{TokioClient, ProxyConfig, ProxySettings, NoProxy};

let settings = ProxySettings::all(
    ProxyConfig::http("http://proxy.example.com:8080").unwrap()
)
.no_proxy(NoProxy::new("localhost, .internal.corp, 10.0.0.0/8"));

let client = TokioClient::builder()
    .proxy_settings(settings)
    .build()?;
```

You can also set different proxies for HTTP and HTTPS:

```rust,no_run
# use aioduct::{TokioClient, ProxyConfig, ProxySettings, NoProxy};
let settings = ProxySettings::default()
    .http(ProxyConfig::http("http://http-proxy:3128").unwrap())
    .https(ProxyConfig::http("http://https-proxy:3129").unwrap())
    .no_proxy(NoProxy::new("localhost"));

let client = TokioClient::builder()
    .proxy_settings(settings)
    .build()?;
```

## Proxy Authentication

Proxy authentication is supported via `basic_auth()`, URI-embedded
credentials, or a credential resolver.

### Explicit Authentication

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};

let client = TokioClient::builder()
    .proxy(
        ProxyConfig::http("http://proxy.example.com:8080")
            .unwrap()
            .basic_auth("user", "pass"),
    )
    .build()?;
```

### Custom CONNECT Headers

For HTTP and HTTPS proxies (which tunnel via `CONNECT`), `header()` attaches
extra headers to the `CONNECT` request — useful for proxy auth tokens or routing
headers beyond Basic auth. SOCKS proxies have no header phase; using CONNECT
headers with a SOCKS proxy fails explicitly when the proxy is used. Different
CONNECT headers segregate pooled connections for HTTP and HTTPS proxies.

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};
use http::header::{HeaderName, HeaderValue};

let client = TokioClient::builder()
    .proxy(
        ProxyConfig::http("http://proxy.example.com:8080")
            .unwrap()
            .header(
                HeaderName::from_static("x-proxy-token"),
                HeaderValue::from_static("secret-token"),
            ),
    )
    .build()?;
```

### Credential Resolver

The `CredentialResolver` trait allows looking up proxy credentials from
external sources. It is called when a proxy has no explicit auth set.

```rust,no_run
use aioduct::{CredentialResolver, ProxyConfig, ProxySettings, TokioClient};

// Built-in: read from environment variables
use aioduct::EnvCredentialResolver;

// Reads AIODUCT_PROXY_USER and AIODUCT_PROXY_PASS globally.
// The `key` parameter (proxy host:port) is reserved for future
// per-proxy resolvers (e.g. platform keychains).
let client = TokioClient::builder()
    .proxy_settings(
        ProxySettings::all(
            ProxyConfig::http("http://proxy:8080").unwrap()
        )
        .proxy_credential_resolver(EnvCredentialResolver),
    )
    .build()?;
```

Composite resolvers try multiple sources in order:

```rust,no_run
use aioduct::{CompositeResolver, EnvCredentialResolver, CredentialResolver};

struct KeychainResolver;
impl CredentialResolver for KeychainResolver {
    fn resolve(&self, key: &str) -> Option<(String, String)> {
        // Look up credentials in the platform keychain by host:port
        None
    }
}

let resolver = CompositeResolver::new()
    .push(KeychainResolver)
    .push(EnvCredentialResolver); // fallback

let client = TokioClient::builder()
    .proxy_settings(
        ProxySettings::all(
            ProxyConfig::http("http://proxy:8080").unwrap()
        )
        .proxy_credential_resolver(resolver),
    )
    .build()?;
```

Priority: `basic_auth()` > URI-embedded credentials > credential resolver.

## Proxy Chaining

Proxy chaining routes requests through multiple proxies in sequence. Each
proxy is reached through the previous one. Up to 2 hops are currently
supported.

```rust,no_run
use aioduct::{ProxyChain, ProxyConfig, TokioClient};

// Chain: client → SOCKS5 exit proxy → corporate HTTP proxy → target
let chain = ProxyChain::new(vec![
    ProxyConfig::socks5("socks5://exit-proxy:1080").unwrap(),
    ProxyConfig::http("http://corporate-proxy:3128")
        .unwrap()
        .basic_auth("employee", "pass"),
]);

let client = TokioClient::builder()
    .proxy_chain(chain)
    .build()?;
```

Any combination of proxy schemes is supported for both hops:

| First Hop | Second Hop | Supported |
|-----------|------------|-----------|
| HTTP | HTTP/HTTPS/SOCKS5/SOCKS5h/SOCKS4 | Yes |
| SOCKS5/SOCKS5h | HTTP/HTTPS/SOCKS5/SOCKS5h/SOCKS4 | Yes |
| SOCKS4 | HTTP/HTTPS/SOCKS5/SOCKS5h/SOCKS4 | Yes |
| HTTPS | HTTP/HTTPS/SOCKS5/SOCKS5h/SOCKS4 | Yes |

When both a proxy chain and a single proxy are configured, the chain
takes priority.

## How It Works

### HTTP Targets

For plain HTTP requests through an HTTP proxy, the client uses a CONNECT
tunnel to the target through the proxy, then sends the request through the
tunnel. This is a transparent tunnel — the proxy relays raw TCP bytes between
the client and the target.

### HTTPS Targets (CONNECT Tunnel)

For HTTPS requests through an HTTP proxy, the client:

1. Connects to the proxy via TCP
2. Sends `CONNECT host:port HTTP/1.1` to establish a tunnel
3. Waits for a successful `2xx` response from the proxy
4. Performs TLS handshake through the tunnel
5. Sends the actual HTTPS request over the encrypted connection

This ensures end-to-end encryption — the proxy only sees the target
hostname, not the request content.

Proxy plans are validated before DNS or TCP I/O. This includes rejecting
non-textual HTTP CONNECT header values, NUL-containing SOCKS4 user IDs,
SOCKS4/SOCKS4a IPv6 targets, and SOCKS5 credentials longer than the protocol's
255-byte fields. DNS, TCP, proxy TLS, CONNECT, and origin TLS observer phases
are emitted when each phase completes, rather than being buffered until the
whole proxy attempt succeeds or fails.

### HTTPS Proxy

When the proxy URL itself uses `https://`, the client wraps the connection
to the proxy in TLS before sending the CONNECT command. This encrypts the
CONNECT handshake (including target hostname and proxy credentials) from
any intermediary between the client and the proxy.

### SOCKS Proxies

SOCKS proxies operate at the TCP level. After the SOCKS handshake (which
establishes a tunnel to the target), the TCP stream is used directly —
for HTTP targets the client sends a normal request, for HTTPS targets TLS
is negotiated over the tunnel.

### HTTP/3 Policy

When a proxy is configured (via `.proxy()`, `.proxy_settings()`, or
`.proxy_chain()`), the client never attempts a direct HTTP/3 connection to
the origin. HTTP/3 proxy tunneling (CONNECT-UDP, RFC 9298) is not yet
supported. Proxied requests use the configured HTTP/1.1 or HTTP/2 tunnel
path instead, even when `.http3(true)` or `.alt_svc_h3(true)` is active on
the builder. Non-proxied requests are unaffected and may use HTTP/3
normally.

## Example: Corporate Proxy

```rust,no_run
use aioduct::{TokioClient, ProxyConfig};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder()
        .proxy(
            ProxyConfig::http("http://corporate-proxy:3128")
                .unwrap()
                .basic_auth("employee", "password"),
        )
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .build()?;

    let resp = client
        .get("https://api.example.com/data")?
        .send()
        .await?;

    println!("{}", resp.text().await?);
    Ok(())
}
```

## CLI Proxy Support

The `aioduct http` and `aioduct download` subcommands support all proxy
schemes via the `-x` / `--proxy` and `--all-proxy` flags:

```sh
# HTTP proxy
aioduct http -x http://proxy:8080 https://example.com

# SOCKS5 proxy
aioduct http -x socks5://127.0.0.1:1080 https://example.com

# SOCKS5h proxy (remote DNS)
aioduct http -x socks5h://proxy:1080 https://internal.corp

# HTTPS proxy
aioduct http -x https://proxy:443 https://example.com

# Multi-hop proxy chaining (repeated -x)
aioduct http -x socks5://gateway:1080 -x http://corp:3128 https://example.com

# Proxy with auth
aioduct http -x http://proxy:8080 --proxy-user admin:secret https://example.com

# System proxy from environment variables
aioduct http --system-proxy https://example.com

# Proxy with bypass rules
aioduct http -x http://proxy:8080 --noproxy localhost,127.0.0.1,.internal \
  https://example.com
```

## Proxy Compatibility Matrix

Every proxy feature is listed below across all runtimes and deployment
targets. Platform-managed transports (wasm, wasi-p2) delegate proxy routing
and bypass to the browser or WASI host; aioduct does not control proxy
behavior on those targets.

| Feature | Tokio / smol / compio | blocking | wasm | wasi-p2 |
|---------|-----------------------|----------|------|---------|
| HTTP proxy (CONNECT tunnel) | Yes | Yes, via wrapped async client | Browser-managed | Host-managed |
| HTTPS proxy (TLS to proxy) | Yes | Yes | Browser-managed | Host-managed |
| SOCKS4 / SOCKS4a | Yes | Yes | Not available | Not available |
| SOCKS5 / SOCKS5h | Yes | Yes | Not available | Not available |
| Proxy auth (Basic, URI-embedded) | Yes | Yes | Browser-managed | Host-managed |
| Credential resolver | Yes | Yes | Not available | Not available |
| Custom CONNECT headers | Yes | Yes | Not available | Not available |
| System proxy (env vars) | Yes | Yes | Not available | Not available |
| NO_PROXY bypass rules | Yes | Yes | Not available | Not available |
| Custom proxy selection | Yes | Yes | Not available | Not available |
| Proxy chaining (up to 2 hops) | Yes | Yes | Not available | Not available |
| HTTP/3 with proxy (CONNECT tunnel fallback) | Yes | Yes | N/A | N/A |
| Redirect through proxy (auth survives hops) | Yes | Yes | Browser-managed | Host-managed |
| TCP keepalive on proxy tunnel | Yes | Yes | N/A | N/A |

### Future Work

| Item | Notes |
|------|-------|
| Windows machine-scope system proxy | System proxy currently means environment variables. Windows registry / WinHTTP proxy discovery is planned. |
| NTLM proxy authentication | Only Basic auth, URI-embedded credentials, and credential resolvers are available. |
| CONNECT-UDP (RFC 9298) | HTTP/3 with a proxy falls back to the configured HTTP/1.1 or HTTP/2 CONNECT tunnel. |
| Per-request proxy override | Proxy configuration is per-client (builder). |

## Limitations

- SOCKS5 supports no-auth and username/password authentication (RFC 1928/1929)
- SOCKS4 supports optional user ID authentication
- Proxy chaining supports up to 2 hops
- CONNECT headers on SOCKS proxies are rejected with a clear error before any I/O.
- HTTP CONNECT headers must contain textual values that can be encoded on the
  HTTP/1.1 CONNECT request.
- SOCKS4 and SOCKS4a cannot carry IPv6 destinations, and SOCKS4 user IDs
  cannot contain NUL bytes.
- SOCKS5 usernames and passwords are limited to 255 bytes each.
- `EnvCredentialResolver` applies the same credentials to all proxies (the
  `key` parameter is reserved for future per-proxy resolvers)
- The HTTP proxy URI must use `http://` or `https://` scheme; SOCKS proxies
  must use `socks4://`, `socks4a://`, `socks5://`, or `socks5h://`
