# Response Decompression

aioduct can automatically decompress response bodies based on the `Content-Encoding` header. Each compression algorithm is gated behind its own feature flag.

## Feature Flags

| Feature   | Codec    | Crate   |
|-----------|----------|---------|
| `gzip`    | gzip     | flate2  |
| `deflate` | deflate  | flate2  |
| `brotli`  | br       | brotli  |
| `zstd`    | zstd     | zstd    |

```toml
[dependencies]
aioduct = { version = "0.1", features = ["tokio", "rustls", "rustls-ring", "gzip", "brotli"] }
```

## How It Works

When any decompression feature is enabled:

1. The client adds an `Accept-Encoding` header to outgoing requests listing the enabled codecs (unless you already set one).
2. If the response has a matching `Content-Encoding`, the body is transparently decompressed.
3. The `Content-Encoding` and `Content-Length` headers are removed from the decompressed response.

```rust,no_run
use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    // With the `gzip` feature enabled, gzip responses are decompressed automatically
    let client = Client::<TokioRuntime>::with_rustls();

    let text = client.get("https://httpbin.org/gzip")?
        .send().await?
        .text().await?;
    println!("{text}");
    Ok(())
}
```

## Disabling Decompression

Use `no_decompression()` on the builder to disable all automatic decompression. The raw compressed bytes are returned as-is.

```rust,no_run
# use aioduct::Client;
# use aioduct::runtime::TokioRuntime;
let client = Client::<TokioRuntime>::builder()
    .no_decompression()
    .build();
```

## Supported Encodings

The `Accept-Encoding` header is built from the enabled features. For example, with `gzip` and `brotli` enabled, outgoing requests include:

```text
Accept-Encoding: zstd, gzip, deflate, br
```

Only codecs whose feature flag is compiled in will appear. If you set `Accept-Encoding` manually on a request, the client will not overwrite it.
