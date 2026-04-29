# Streaming Downloads

aioduct supports streaming response bodies chunk-by-chunk, avoiding the need to buffer the entire response in memory. This is essential for downloading large files.

## BodyStream

Convert a response into a `BodyStream` that yields `Bytes` chunks:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get("http://example.com/large-file.bin")?
        .send()
        .await?;

    let mut stream = resp.into_bytes_stream();
    let mut total = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total += chunk.len();
        // process chunk...
    }

    println!("downloaded {total} bytes");
    Ok(())
}
```

## Streaming to a File

Combine `BodyStream` with `tokio::fs::File` to download directly to disk:

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::<TokioRuntime>::new();

    let resp = client
        .get("http://example.com/large-file.bin")?
        .send()
        .await?;

    let mut file = tokio::fs::File::create("output.bin").await?;
    let mut stream = resp.into_bytes_stream();

    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;

    Ok(())
}
```

## Choosing Between Methods

| Method | Use Case | Memory |
|--------|----------|--------|
| `resp.bytes()` | Small responses, read all at once | Entire body in memory |
| `resp.text()` | Small text responses | Entire body in memory |
| `resp.into_bytes_stream()` | Large downloads, progress tracking | One chunk at a time |
| `resp.into_sse_stream()` | Server-Sent Events | One event at a time |
