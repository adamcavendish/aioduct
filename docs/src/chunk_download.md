# Parallel Chunk Download

aioduct supports parallel chunk download for large files by splitting the download into multiple HTTP Range requests fetched concurrently. This can significantly improve download speed when the server supports range requests.

## Basic Usage

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    let result = client
        .chunk_download("http://example.com/large-file.bin")
        .chunks(8)
        .download()
        .await?;

    println!("Downloaded {} bytes", result.total_size);
    // result.data contains the reassembled file
    Ok(())
}
```

## How It Works

1. **HEAD request** — checks `Accept-Ranges: bytes` and `Content-Length` headers
2. **Range splitting** — divides the file into N equal-sized chunks
3. **Parallel fetch** — spawns concurrent `Range` requests via the runtime
4. **Reassembly** — collects chunks in order and concatenates them

If the server doesn't support range requests (no `Accept-Ranges: bytes` header or missing `Content-Length`), the download falls back to a single GET request.

## Configuration

| Method | Default | Description |
|--------|---------|-------------|
| `.chunks(n)` | 4 | Number of parallel range requests |

## Result

`ChunkDownloadResult` contains:

- `total_size: u64` — the total file size in bytes
- `data: Bytes` — the complete downloaded content

## Server Requirements

For parallel download to activate, the server must:

- Respond to HEAD with `Accept-Ranges: bytes`
- Include a `Content-Length` header
- Support `Range: bytes=start-end` requests and respond with `206 Partial Content`

## Example: Download and Save to File

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::<TokioRuntime>::new();

    let result = client
        .chunk_download("http://example.com/large-file.zip")
        .chunks(8)
        .download()
        .await?;

    let mut file = tokio::fs::File::create("large-file.zip").await?;
    file.write_all(&result.data).await?;

    println!("Downloaded {} bytes", result.total_size);
    Ok(())
}
```

## Notes

- The `Client` is cloned (cheaply — all internal state is behind `Arc`) for each parallel task
- If any chunk request fails, the entire download fails
- The number of chunks is capped at the total file size (1-byte minimum per chunk)
