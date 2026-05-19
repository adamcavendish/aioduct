use aioduct::BlockingCompioClient;
use aioduct::CompioClient;

fn main() -> Result<(), aioduct::Error> {
    let engine = CompioClient::builder().build_local().unwrap();
    let client = BlockingCompioClient::new(engine);

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
