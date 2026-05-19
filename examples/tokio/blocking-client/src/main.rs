use aioduct::BlockingTokioClient;
use aioduct::TokioClient;

fn main() -> Result<(), aioduct::Error> {
    let engine = TokioClient::builder().build().unwrap();
    let client = BlockingTokioClient::new(engine);

    // Synchronous GET
    let resp = client.get("https://httpbin.org/get")?.send()?;
    println!("Status: {}", resp.status());
    println!("Body:\n{}", resp.text()?);

    // Synchronous POST
    let resp = client
        .post("https://httpbin.org/post")?
        .body("hello from blocking client")
        .send()?;

    println!("\nPOST status: {}", resp.status());
    println!("Body:\n{}", resp.text()?);

    Ok(())
}
