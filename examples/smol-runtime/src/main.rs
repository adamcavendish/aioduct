use aioduct::Client;
use aioduct::runtime::SmolRuntime;

fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = Client::<SmolRuntime>::builder().build();

        let resp = client.get("https://httpbin.org/get")?.send().await?;

        println!("Status: {}", resp.status());
        println!("Version: {:?}", resp.version());
        println!("Body:\n{}", resp.text().await?);

        // Concurrent requests with smol
        let (r1, r2) = smol::future::zip(
            async { client.get("https://httpbin.org/get").unwrap().send().await },
            async { client.get("https://httpbin.org/ip").unwrap().send().await },
        )
        .await;

        println!("\nConcurrent request 1: {}", r1?.status());
        println!("Concurrent request 2: {}", r2?.status());

        Ok(())
    })
}
