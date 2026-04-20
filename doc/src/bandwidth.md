# Bandwidth Limiting

aioduct provides a token-bucket bandwidth limiter for throttling download speed. Unlike the [`RateLimiter`](https://docs.rs/aioduct/latest/aioduct/struct.RateLimiter.html) which limits requests per second, the bandwidth limiter limits bytes per second.

## Usage

Set a maximum download speed at the client level:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .max_download_speed(1_048_576) // 1 MB/s
    .build();

let resp = client
    .get("https://example.com/large-file.tar.gz")?
    .send()
    .await?;
```

## How It Works

The limiter uses a token-bucket algorithm:

1. The bucket starts full — capacity equals `bytes_per_sec`.
2. Each time data is read from a response body, the limiter checks if enough tokens are available.
3. If tokens are available, the read proceeds immediately and tokens are consumed.
4. If the bucket is empty, the read is delayed until tokens refill.
5. Tokens refill continuously at the configured byte rate.

## API

The `BandwidthLimiter` is also available as a standalone type:

```rust,no_run
use aioduct::BandwidthLimiter;
use std::time::Duration;

let limiter = BandwidthLimiter::new(100_000); // 100 KB/s

// Try to consume bytes (non-blocking)
let granted = limiter.try_consume(8192);

// Check how long to wait for more bytes
let wait = limiter.wait_duration(8192);
```

| Method | Description |
|--------|-------------|
| `try_consume(n)` | Consume up to `n` bytes, returns bytes actually granted (may be 0) |
| `wait_duration(n)` | Duration to wait before `n` bytes become available |

## Shared State

`BandwidthLimiter` uses `Arc` internally, so cloning shares the same token bucket. This means the limit is enforced globally across all concurrent requests on the same client.
