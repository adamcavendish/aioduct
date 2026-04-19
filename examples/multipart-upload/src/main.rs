use aioduct::runtime::TokioRuntime;
use aioduct::{Client, Multipart, Part};

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    // Build a multipart form with text and file parts
    let form = Multipart::new()
        .text("field1", "hello world")
        .text("field2", "another value")
        .part(
            Part::bytes("file", b"file content here".to_vec())
                .file_name("example.txt")
                .mime_str("text/plain"),
        );

    let resp = client
        .post("https://httpbin.org/post")?
        .multipart(form)
        .send()
        .await?;

    println!("Status: {}", resp.status());
    println!("Response:\n{}", resp.text().await?);

    // Multipart with binary data
    let png_header = vec![0x89u8, 0x50, 0x4E, 0x47]; // PNG magic bytes
    let form = Multipart::new().text("description", "a fake image").part(
        Part::bytes("image", png_header)
            .file_name("image.png")
            .mime_str("image/png"),
    );

    let resp = client
        .post("https://httpbin.org/post")?
        .multipart(form)
        .send()
        .await?;

    println!("\nBinary upload status: {}", resp.status());

    Ok(())
}
