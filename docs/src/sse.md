# Server-Sent Events (SSE)

aioduct has built-in support for consuming [Server-Sent Events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events) streams. SSE is a standard for servers to push events to clients over HTTP, commonly used by LLM APIs (OpenAI, Anthropic) for streaming responses.

## Basic Usage

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get("http://example.com/events")?
        .send()
        .await?;

    let mut sse = resp.into_sse_stream();
    while let Some(event) = sse.next().await {
        let event = event?;
        println!("event: {:?}, data: {}", event.event, event.data);
    }

    Ok(())
}
```

## SseEvent Fields

Each parsed event contains:

| Field   | Type             | Description                                |
|---------|------------------|--------------------------------------------|
| `event` | `Option<String>` | Event type (from `event:` field)           |
| `data`  | `String`         | Event payload (joined with `\n` for multi-line) |
| `id`    | `Option<String>` | Event ID (from `id:` field)                |
| `retry` | `Option<u64>`    | Reconnection time in ms (from `retry:` field) |

## SSE Wire Format

The SSE protocol uses a simple text-based format where events are separated by blank lines (`\n\n`):

```text
event: greeting
data: hello

data: line1
data: line2

event: done
data: bye
id: 42
retry: 5000

```

This produces three events:
1. `SseEvent { event: Some("greeting"), data: "hello", id: None, retry: None }`
2. `SseEvent { event: None, data: "line1\nline2", id: None, retry: None }`
3. `SseEvent { event: Some("done"), data: "bye", id: Some("42"), retry: Some(5000) }`

## Comments

Lines starting with `:` are comments and are silently ignored:

```text
: this is a heartbeat comment
data: actual event

```

## Example: Streaming LLM API

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::with_rustls();

    let resp = client
        .post("https://api.example.com/v1/chat/completions")?
        .bearer_auth("sk-...")
        .header_str("content-type", "application/json")?
        .body(r#"{"model":"gpt-4","stream":true,"messages":[{"role":"user","content":"Hi"}]}"#)
        .send()
        .await?;

    let mut sse = resp.into_sse_stream();
    while let Some(event) = sse.next().await {
        let event = event?;
        if event.data == "[DONE]" {
            break;
        }
        print!("{}", event.data);
    }

    Ok(())
}
```
