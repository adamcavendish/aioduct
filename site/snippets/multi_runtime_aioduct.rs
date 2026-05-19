// features: smol
// runtime: smol
use aioduct::SmolClient;

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = SmolClient::builder().build()?;

        let resp = client.get("http://httpbin.org/get")?.send().await?;
        println!("Status: {}", resp.status());
        Ok(())
    })
}
