#[path = "../../smol_threads.rs"]
mod smol_threads;

use aioduct::BlockingSmolClient;
use aioduct::SmolClient;

fn main() -> Result<(), aioduct::Error> {
    smol_threads::configure();
    let engine = SmolClient::builder().build().unwrap();
    let client = BlockingSmolClient::new(engine);

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
