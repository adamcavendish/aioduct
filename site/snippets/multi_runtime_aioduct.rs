// features: smol
// runtime: smol
use aioduct::Client;
use aioduct::runtime::SmolRuntime;

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = Client::<SmolRuntime>::builder().build();

        let resp = client.get("http://httpbin.org/get")?.send().await?;
        println!("Status: {}", resp.status());
        Ok(())
    })
}
