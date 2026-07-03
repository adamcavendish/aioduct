# Streaming Downloads

aioduct supports streaming response bodies chunk-by-chunk, avoiding the need to buffer the entire response in memory. This is essential for downloading large files.

## BodyStream

Convert a response into a `BodyStream` that yields `Bytes` chunks:

```rust,no_run
use aioduct::TokioClient;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::new();

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

After the stream is exhausted, call `trailers()` to inspect any HTTP trailers
captured from the response:

```rust,no_run
# use aioduct::TokioClient;
# #[tokio::main]
# async fn main() -> Result<(), aioduct::Error> {
# let client = TokioClient::new();
# let resp = client.get("http://example.com/with-trailers")?.send().await?;
let mut stream = resp.into_bytes_stream();
while let Some(chunk) = stream.next().await {
    let _ = chunk?;
}

if let Some(trailers) = stream.trailers() {
    println!("trailers: {trailers:?}");
}
# Ok(())
# }
```

## Streaming to a File

Combine `BodyStream` with `tokio::fs::File` to download directly to disk:

```rust,no_run
use aioduct::TokioClient;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = TokioClient::new();

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
