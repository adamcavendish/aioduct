# HSTS (HTTP Strict Transport Security)

aioduct supports automatic HTTP-to-HTTPS upgrade via the `Strict-Transport-Security` header (RFC 6797). When a server sends this header over HTTPS, subsequent HTTP requests to that domain are transparently upgraded to HTTPS.

## Enabling HSTS

Create an `HstsStore` and pass it to the client builder:

```rust,no_run
use aioduct::{Client, HstsStore};
use aioduct::runtime::TokioRuntime;

let hsts = HstsStore::new();
let client = Client::<TokioRuntime>::builder()
    .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
    .hsts(hsts)
    .build();
```

## How It Works

1. When an HTTPS response contains a `Strict-Transport-Security` header, the domain and its policy are recorded in the store
2. On subsequent requests to the same domain over `http://`, the URL is transparently upgraded to `https://`
3. If the header includes `includeSubDomains`, all subdomains of the host are also upgraded
4. A `max-age=0` directive removes the domain from the store

## Header Format

```text
Strict-Transport-Security: max-age=31536000
Strict-Transport-Security: max-age=31536000; includeSubDomains
```

- `max-age` — how long (in seconds) the browser/client should remember to use HTTPS
- `includeSubDomains` — also apply the policy to all subdomains

## Subdomain Matching

When `includeSubDomains` is set for `example.com`:

- `http://example.com` → upgraded to `https://example.com`
- `http://api.example.com` → upgraded to `https://api.example.com`
- `http://deep.sub.example.com` → upgraded to `https://deep.sub.example.com`

Without `includeSubDomains`, only the exact domain is upgraded.

## Shared State

`HstsStore` uses `Arc<Mutex<...>>` internally, so cloning a store shares state between clients:

```rust
# use aioduct::HstsStore;
let store = HstsStore::new();
let store2 = store.clone(); // shares the same data
```

## Clearing the Store

```rust
# use aioduct::HstsStore;
let store = HstsStore::new();
// ... use with client ...
store.clear();
```
