# Streaming Uploads

aioduct supports streaming request bodies for large file uploads without buffering the entire content in memory. This is useful for uploading files larger than available RAM or when the content size isn't known upfront.

## RequestBody

Internally, request bodies are represented as `RequestBody`, which has two variants:

- **Buffered** — an in-memory `Bytes` buffer (used by `.body()`, `.json()`, `.form()`, `.multipart()`)
- **Streaming** — a `AioductBody` that produces chunks on demand

Buffered bodies can be retried and redirected automatically. Streaming bodies are consumed on first use — retries and 307/308 redirects that preserve the body will send an empty body on subsequent attempts.

## Basic Streaming Upload

```rust,no_run
use aioduct::{Client, AioductBody};
use aioduct::runtime::TokioRuntime;
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use futures_util::stream;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    // Create a stream of body frames
    let chunks = vec![
        Ok(hyper::body::Frame::data(Bytes::from("chunk 1 "))),
        Ok(hyper::body::Frame::data(Bytes::from("chunk 2 "))),
        Ok(hyper::body::Frame::data(Bytes::from("chunk 3"))),
    ];
    let body: AioductBody = StreamBody::new(stream::iter(chunks)).boxed();

    let resp = client
        .post("http://httpbin.org/post")?
        .body_stream(body)
        .send()
        .await?;

    println!("status: {}", resp.status());
    Ok(())
}
```

## Streaming from a File

```rust,no_run
use aioduct::{Client, AioductBody};
use aioduct::runtime::TokioRuntime;
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .build();

    let file = tokio::fs::File::open("large_file.bin").await.unwrap();
    let reader = tokio::io::BufReader::new(file);
    let stream = tokio_util::io::ReaderStream::new(reader);
    let mapped = futures_util::StreamExt::map(stream, |result| {
        result
            .map(|bytes| hyper::body::Frame::data(bytes))
            .map_err(|e| aioduct::Error::Io(e))
    });
    let body: AioductBody = StreamBody::new(mapped).boxed();

    let resp = client
        .put("https://httpbin.org/put")?
        .body_stream(body)
        .send()
        .await?;

    println!("status: {}", resp.status());
    Ok(())
}
```

## Buffered vs Streaming

| Feature | `.body()` (Buffered) | `.body_stream()` (Streaming) |
|---------|---------------------|------------------------------|
| Memory  | Entire body in RAM  | Chunk at a time              |
| Retry   | Full retry support  | First attempt only           |
| Redirect (307/308) | Body preserved | Body consumed |
| Redirect (301/302/303) | Body dropped (GET) | Body dropped (GET) |

## When to Use Streaming

- Uploading files larger than available memory
- Proxying data from one source to another
- Generating body content dynamically (e.g., from a database cursor)

For small payloads, `.body()` is simpler and supports automatic retries.
