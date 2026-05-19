# HTTP Caching

aioduct includes an in-memory HTTP cache that respects `Cache-Control` directives, conditional validation with `ETag`/`If-None-Match` and `Last-Modified`/`If-Modified-Since`, and stale content extensions.

## Enabling the Cache

```rust,no_run
use aioduct::{TokioClient, HttpCache};
use aioduct::runtime::tokio_rt::TcpConnector;

let cache = HttpCache::new();
let client = TokioClient::builder(TcpConnector)
    .cache(cache)
    .build()?;
```

## Cache-Control Directives

The cache respects standard directives from RFC 9111:

| Directive | Behavior |
|-----------|----------|
| `max-age=N` | Response is fresh for N seconds |
| `s-maxage=N` | Shared cache max-age (takes precedence over `max-age`) |
| `no-cache` | Always revalidate before serving |
| `no-store` | Never store the response |
| `must-revalidate` | Must revalidate once stale |
| `private` | Response is not cacheable by shared caches |

## Immutable Responses (RFC 8246)

Responses with `Cache-Control: immutable` are never revalidated while fresh. This is useful for content-addressed resources (e.g., `/assets/app-abc123.js`) that never change at the same URL.

```text
Cache-Control: max-age=31536000, immutable
```

The cache skips conditional requests entirely for these entries.

## Stale Content Extensions (RFC 5861)

### stale-while-revalidate

Allows the cache to serve a stale response while asynchronously revalidating in the background:

```text
Cache-Control: max-age=60, stale-while-revalidate=30
```

The response is served fresh for 60 seconds, then served stale for up to 30 more seconds while a background revalidation occurs.

### stale-if-error

Allows the cache to serve a stale response when the origin server returns a 5xx error or is unreachable:

```text
Cache-Control: max-age=60, stale-if-error=3600
```

If the origin is unavailable, the stale response can be served for up to 3600 seconds past expiry.

**Client behavior:** When the client holds a stale cached response with a `stale-if-error` directive, it first attempts a normal request to the origin (including conditional validation headers). If the origin returns a 5xx status code or the connection fails entirely, the client checks whether the cached entry's age is within the `stale-if-error` grace window. If so, the stale cached response is returned transparently instead of the error. If the grace window has expired, the original error is propagated.

## Conditional Validation

When a cached response becomes stale, the cache performs conditional validation:

1. If the cached response has an `ETag`, the request includes `If-None-Match`
2. If the cached response has a `Last-Modified` date, the request includes `If-Modified-Since`
3. A `304 Not Modified` response refreshes the cache entry without transferring the body

## Cache Configuration

`CacheConfig` controls cache behavior:

```rust
use aioduct::{CacheConfig, HttpCache};

let cache = HttpCache::with_config(CacheConfig::default());
```

| Method | Default | Description |
|--------|---------|-------------|
| `max_entries()` | `256` | Maximum number of cached responses |

## What Gets Cached

Only responses to safe, idempotent methods (`GET`, `HEAD`) with cacheable status codes (200, 301, etc.) are cached. Unsafe methods (`POST`, `PUT`, `DELETE`, `PATCH`) invalidate matching cache entries.

## Shared State

`HttpCache` uses `Arc` internally, so cloning shares state between clients:

```rust
# use aioduct::HttpCache;
let cache = HttpCache::new();
let cache2 = cache.clone(); // shares the same data
```

## Custom Cache Store

Implement the `CacheStore` trait to plug in a custom backend (moka, foyer, Redis, etc.):

```rust,no_run
use aioduct::{CacheStore, CacheEntry, HttpCache, TokioClient};
use aioduct::runtime::tokio_rt::TcpConnector;
use http::{Method, Uri};

struct MyCacheStore { /* ... */ }

impl CacheStore for MyCacheStore {
    fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry> {
        // look up entry
        # None
    }
    fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
        // store entry
    }
    fn remove(&self, method: &Method, uri: &Uri) {
        // remove entry
    }
    fn clear(&self) {
        // clear all entries
    }
    fn len(&self) -> usize {
        // return count
        # 0
    }
}

let cache = HttpCache::with_store(MyCacheStore { /* ... */ });
let client = TokioClient::builder(TcpConnector)
    .cache(cache)
    .build()?;
```

The built-in `InMemoryCacheStore` is available if you need a reference implementation or want to wrap it with additional behavior (metrics, logging, etc.).
