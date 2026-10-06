#[path = "../../smol_threads.rs"]
mod smol_threads;

use aioduct::SmolClient;
fn main() -> Result<(), aioduct::Error> {
    smol_threads::configure();
    smol::block_on(async {
        let client = SmolClient::builder().build().unwrap();
        let _resp = client
            .get("http://localhost:8080/")?
            .h2c_prior_knowledge()
            .send()
            .await?;
        Ok(())
    })
}
