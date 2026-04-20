# Digest Authentication

aioduct supports HTTP Digest Authentication (RFC 7616). When configured, the client automatically handles the 401 challenge-response flow — no manual header construction needed.

## How It Works

1. The client sends the initial request without credentials.
2. If the server responds with `401 Unauthorized` and a `WWW-Authenticate: Digest ...` header, the client parses the challenge.
3. The client computes the digest response using the MD5 algorithm, the request method, URI, and the server-provided nonce.
4. The request is retried with the `Authorization: Digest ...` header.

This is a single automatic retry — if the second request also returns 401, it is returned as-is.

## Usage

Configure digest auth at the client level:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .digest_auth("username", "password")
    .build();

// The client handles the 401 → retry flow automatically
let resp = client
    .get("https://example.com/protected")?
    .send()
    .await?;
```

## Supported Features

| Feature | Status |
|---------|--------|
| MD5 algorithm | Supported |
| `qop=auth` | Supported |
| `opaque` parameter | Supported |
| Nonce counting (`nc`) | Supported |
| Client nonce (`cnonce`) | Supported |
| MD5-sess | Not supported |
| SHA-256 | Not supported |
| `qop=auth-int` | Not supported |

## Implementation Notes

- The MD5 implementation is pure Rust with no external dependency.
- The nonce counter is atomic, so digest auth is safe to use from concurrent requests.
- Digest auth runs after the initial request completes but before redirect handling, so it works correctly with redirect-protected resources.
- The client nonce is generated using `RandomState` for uniqueness without requiring a CSPRNG dependency.
