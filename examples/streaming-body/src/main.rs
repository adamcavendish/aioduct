use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    // Streaming response — read body as an async byte stream
    let resp = client
        .get("https://httpbin.org/stream-bytes/1024")?
        .send()
        .await?;

    println!("Status: {}", resp.status());

    // The response can be converted to a byte stream via into_bytes_stream().
    // The returned BodyStream implements futures_core::Stream<Item = Result<Bytes, Error>>,
    // so you can use StreamExt from the futures crate to consume it.
    // For simplicity, we just collect all bytes at once here:
    let resp = client
        .get("https://httpbin.org/stream-bytes/2048")?
        .send()
        .await?;

    let bytes = resp.bytes().await?;
    println!("Downloaded {} bytes", bytes.len());

    // Streaming upload from a byte vector
    let resp = client
        .post("https://httpbin.org/post")?
        .body(vec![0u8; 4096])
        .send()
        .await?;

    println!("\nUpload status: {}", resp.status());
    println!("Response length: {:?}", resp.content_length());

    Ok(())
}
