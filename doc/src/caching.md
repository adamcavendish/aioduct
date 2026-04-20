# HTTP Caching

aioduct includes an in-memory HTTP cache that respects `Cache-Control` directives, conditional validation with `ETag`/`If-None-Match` and `Last-Modified`/`If-Modified-Since`, and stale content extensions.

## Enabling the Cache

```rust,no_run
use aioduct::{Client, HttpCache, CacheConfig};
use aioduct::runtime::TokioRuntime;

let cache = HttpCache::new(CacheConfig::default());
let client = Client::<TokioRuntime>::builder()
    .cache(cache)
    .build();
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

## Conditional Validation

When a cached response becomes stale, the cache performs conditional validation:

1. If the cached response has an `ETag`, the request includes `If-None-Match`
2. If the cached response has a `Last-Modified` date, the request includes `If-Modified-Since`
3. A `304 Not Modified` response refreshes the cache entry without transferring the body

## Cache Configuration

`CacheConfig` controls cache behavior:

```rust
use aioduct::CacheConfig;

let config = CacheConfig::default()
    .max_entries(500);
```

| Method | Default | Description |
|--------|---------|-------------|
| `max_entries()` | `1000` | Maximum number of cached responses |

## What Gets Cached

Only responses to safe, idempotent methods (`GET`, `HEAD`) with cacheable status codes (200, 301, etc.) are cached. Unsafe methods (`POST`, `PUT`, `DELETE`, `PATCH`) invalidate matching cache entries.

## Shared State

`HttpCache` uses `Arc<Mutex<...>>` internally, so cloning shares state between clients:

```rust
# use aioduct::{HttpCache, CacheConfig};
let cache = HttpCache::new(CacheConfig::default());
let cache2 = cache.clone(); // shares the same data
```
