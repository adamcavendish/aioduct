# Multipart/Form-Data

aioduct supports building `multipart/form-data` request bodies for file uploads and mixed form submissions.

## Basic Usage

```rust,no_run
use aioduct::{Client, Multipart};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    let form = Multipart::new()
        .text("username", "alice")
        .text("description", "Profile photo");

    let resp = client
        .post("http://example.com/upload")?
        .multipart(form)
        .send()
        .await?;

    println!("status: {}", resp.status());
    Ok(())
}
```

## Text Fields

Add plain text form fields with `.text(name, value)`:

```rust
# use aioduct::Multipart;
let form = Multipart::new()
    .text("field1", "value1")
    .text("field2", "value2");
```

## File Parts

Add file parts with `.file(name, filename, content_type, data)`:

```rust
# use aioduct::Multipart;
let form = Multipart::new()
    .text("description", "My document")
    .file("document", "report.pdf", "application/pdf", include_bytes!("../../Cargo.toml").as_slice());
```

The `data` parameter accepts anything that implements `Into<Bytes>` — `&[u8]`, `Vec<u8>`, `String`, `Bytes`, etc.

## Mixed Forms

Combine text fields and file parts freely:

```rust,no_run
use aioduct::{Client, Multipart};
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::new();

    let image_data = std::fs::read("photo.jpg").unwrap();

    let form = Multipart::new()
        .text("title", "Vacation photo")
        .text("album", "Summer 2025")
        .file("photo", "photo.jpg", "image/jpeg", image_data);

    let resp = client
        .post("http://example.com/api/photos")?
        .multipart(form)
        .send()
        .await?;

    println!("uploaded: {}", resp.status());
    Ok(())
}
```

## Wire Format

The generated body follows RFC 2046 multipart encoding:

```text
------aioduct<boundary>\r\n
Content-Disposition: form-data; name="field1"\r\n
\r\n
value1\r\n
------aioduct<boundary>\r\n
Content-Disposition: form-data; name="file"; filename="photo.jpg"\r\n
Content-Type: image/jpeg\r\n
\r\n
<binary data>\r\n
------aioduct<boundary>--\r\n
```

The boundary is auto-generated per `Multipart` instance. The `Content-Type` header is set automatically when using `.multipart()` on the request builder.
