# Retry with Backoff

aioduct supports automatic retries with configurable exponential backoff. Retries can be set at the client level (applied to all requests) or per-request.

## Basic Usage

```rust,no_run
use std::time::Duration;
use aioduct::{Client, RetryConfig};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get("http://example.com/api")?
        .retry(RetryConfig::default())
        .send()
        .await?;

    println!("status: {}", resp.status());
    Ok(())
}
```

## RetryConfig

| Field               | Type       | Default  | Description                                  |
|---------------------|------------|----------|----------------------------------------------|
| `max_retries`       | `u32`      | `3`      | Maximum number of retry attempts             |
| `initial_backoff`   | `Duration` | `100ms`  | Delay before the first retry                 |
| `max_backoff`       | `Duration` | `30s`    | Upper bound on backoff delay                 |
| `backoff_multiplier`| `f64`      | `2.0`    | Multiplier applied to backoff each attempt   |
| `retry_on_status`   | `bool`     | `true`   | Whether to retry on 5xx server errors        |

The delay for attempt *n* (0-indexed) is:

```text
delay = min(initial_backoff * multiplier^n, max_backoff)
```

## What Gets Retried

By default, aioduct retries on:
- **Connection errors** — I/O errors, hyper transport errors
- **Timeouts** — request-level or client-level timeout exceeded
- **5xx server errors** — 500, 502, 503, etc. (when `retry_on_status` is true)

Client errors (4xx) are never retried. To disable 5xx retry, set `retry_on_status(false)`.

## Client-Level Retry

Set a default retry policy for all requests:

```rust,no_run
use std::time::Duration;
use aioduct::{Client, RetryConfig};
use aioduct::runtime::TokioRuntime;

let client = Client::<TokioRuntime>::builder()
    .retry(
        RetryConfig::default()
            .max_retries(5)
            .initial_backoff(Duration::from_millis(200))
            .max_backoff(Duration::from_secs(10)),
    )
    .build();
```

## Per-Request Override

A retry config on a request takes precedence over the client default:

```rust,no_run
# use std::time::Duration;
# use aioduct::{Client, RetryConfig};
# use aioduct::runtime::TokioRuntime;
# let client = Client::<TokioRuntime>::new();
let resp = client
    .post("http://example.com/idempotent-endpoint")?
    .retry(RetryConfig::default().max_retries(1))
    .body("payload")
    .send()
    .await?;
# Ok::<_, aioduct::Error>(())
```

## Example: Resilient LLM API Client

```rust,no_run
use std::time::Duration;
use aioduct::{Client, RetryConfig};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .retry(
            RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(500))
                .backoff_multiplier(2.0),
        )
        .timeout(Duration::from_secs(30))
        .build();

    let resp = client
        .post("https://api.example.com/v1/chat/completions")?
        .bearer_auth("sk-...")
        .header_str("content-type", "application/json")?
        .body(r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hi"}]}"#)
        .send()
        .await?;

    println!("{}", resp.text().await?);
    Ok(())
}
```
