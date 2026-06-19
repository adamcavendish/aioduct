# Retry with Backoff

aioduct supports automatic retries with configurable exponential backoff. Retries can be set at the client level (applied to all requests) or per-request.

## Basic Usage

```rust,no_run
use std::time::Duration;
use aioduct::{TokioClient, RetryConfig};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::new();

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
| `retry_on_status`   | `bool`     | `true`   | Whether to retry on retryable HTTP statuses  |
| `budget`            | `Option<RetryBudget>` | `None` | Token-bucket budget to prevent retry storms |

The delay for attempt *n* (0-indexed) is:

```text
delay = min(initial_backoff * multiplier^n, max_backoff)
```

## What Gets Retried

By default, aioduct retries on:
- **Connection errors** — I/O errors, hyper transport errors
- **Timeouts** — overall request, connect, and response read-gap timeouts
- **5xx server errors** — 500, 502, 503, etc. (when `retry_on_status` is true)
- **429 Too Many Requests** — rate limiting responses (when `retry_on_status` is true)
- **408 Request Timeout** and **425 Too Early** — retried for idempotent requests (when `retry_on_status` is true)

Status-based retries are only attempted for idempotent methods: GET, HEAD, PUT,
DELETE, OPTIONS, and TRACE. POST and PATCH are not retried on status by the
default classifier. Other client errors are never retried. To disable all
status-based retry, set `retry_on_status(false)`.

## Custom Classifier

For policies the built-in rules do not cover, attach a classifier with
`classify()`. The closure receives a `RetryContext` describing the outcome (a
response status or a transport error), the request method, and the attempt
counters, and returns a `RetryDecision`:

- `RetryDecision::Retry` — retry, still bounded by `max_retries` and any
  `budget`. This is an explicit opt-in, so it applies even to non-idempotent
  methods like POST.
- `RetryDecision::DoNotRetry` — stop and return the response or error.
- `RetryDecision::UseDefault` — defer to the built-in classification.

```rust,no_run
use aioduct::{TokioClient, RetryConfig, RetryDecision, RetryOutcome};

# fn build() -> Result<(), aioduct::Error> {
let client = TokioClient::builder()
    .retry(RetryConfig::default().classify(|ctx| match ctx.outcome() {
        // Retry a normally-final 404 (e.g. eventually-consistent resource).
        RetryOutcome::Status(s) if s.as_u16() == 404 => RetryDecision::Retry,
        // Never retry 503 for this client.
        RetryOutcome::Status(s) if s.as_u16() == 503 => RetryDecision::DoNotRetry,
        // Everything else keeps the built-in behavior.
        _ => RetryDecision::UseDefault,
    }))
    .build()?;
# let _ = client;
# Ok(())
# }
```

Returning `UseDefault` for every outcome leaves behavior identical to having no
classifier. The classifier runs before the built-in rules on every attempt, for
both status responses and transport errors.

## Retry-After Header

When a server responds with a `Retry-After` header on a retryable status response (common on 429 and 503 responses), aioduct uses the server's requested delay instead of its own exponential backoff for that attempt. Both formats are supported:

- **Seconds**: `Retry-After: 120` — wait 120 seconds
- **HTTP-date**: `Retry-After: Wed, 21 Oct 2026 07:28:00 GMT` — wait until the specified time

If the `Retry-After` value is missing or unparseable, the normal backoff delay is used.

`Retry-After` is only considered after a retryable status response. Transport
errors and timeout retries use the configured exponential backoff.

## Policy Boundaries

Retries are scoped to the request builder that enabled the policy. A retryable
final response after redirects causes the whole request operation to be tried
again, including any redirect hops needed to reach the final target. Redirect
limits and timeout limits still apply normally on every attempt.

The request `timeout()` is per attempt when retries are enabled. Backoff sleeps
and later retry attempts can make total wall-clock time exceed that duration.

Streaming request bodies are not replayed after they have been consumed. Use a
buffered body when a request must be safely replayable, or use a custom
classifier only when the application can prove the operation is safe to repeat.

## Retry Budget

A `RetryBudget` prevents retry storms by limiting the total retry rate across all requests. Each successful (non-retried) request deposits tokens; each retry attempt withdraws one. When the budget is exhausted, retries are suppressed.

```rust,no_run
use std::time::Duration;
use aioduct::{TokioClient, RetryConfig, RetryBudget};

let client = TokioClient::builder()
    .retry(
        RetryConfig::default()
            .budget(RetryBudget::new(10, 1)),  // max 10 tokens, +1 per success
    )
    .build()?;
```

## Client-Level Retry

Set a default retry policy for all requests:

```rust,no_run
use std::time::Duration;
use aioduct::{TokioClient, RetryConfig};

let client = TokioClient::builder()
    .retry(
        RetryConfig::default()
            .max_retries(5)
            .initial_backoff(Duration::from_millis(200))
            .max_backoff(Duration::from_secs(10)),
    )
    .build()?;
```

## Per-Request Override

A retry config on a request takes precedence over the client default:

```rust,no_run
# use std::time::Duration;
# use aioduct::{TokioClient, RetryConfig};
# let client = TokioClient::new();
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
use aioduct::{TokioClient, RetryConfig};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder()
        .retry(
            RetryConfig::default()
                .max_retries(3)
                .initial_backoff(Duration::from_millis(500))
                .backoff_multiplier(2.0),
        )
        .timeout(Duration::from_secs(30))
        .build()?;

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
