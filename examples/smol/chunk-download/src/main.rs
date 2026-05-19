use std::time::Duration;

use aioduct::SmolClient;
fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = SmolClient::builder()
            .max_download_speed(5_000_000) // 5 MB/s shared across all parallel chunks
            .max_requests_per_sec(20) // rate-limit HEAD + Range requests
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        // httpbin /range/{n} supports Accept-Ranges: bytes, ideal for chunk download demo
        let url = "https://httpbin.org/range/10240";

        // Parallel chunk download — splits the file into ranges and downloads concurrently
        println!("Starting parallel chunk download...");
        println!("URL: {url}");

        let result = client
            .chunk_download(url)
            .chunks(4) // 4 parallel range requests
            .download()
            .await?;

        println!("Total size: {} bytes", result.total_size);
        println!("Data length: {} bytes", result.data.len());

        // Verify data integrity
        assert_eq!(result.total_size as usize, result.data.len());
        println!("Download complete and verified!");

        Ok(())
    })
}
