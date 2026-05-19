use std::time::Duration;

use aioduct::CompioClient;

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = CompioClient::builder()
            .timeout(Duration::from_secs(30))
            .build_local()
            .unwrap();

        // httpbin /range/{n} supports Accept-Ranges: bytes, ideal for chunk download demo
        let url = "https://httpbin.org/range/10240";

        println!("Starting parallel chunk download...");
        println!("URL: {url}");

        let result = client
            .chunk_download_local(url)
            .chunks(4)
            .download()
            .await?;

        println!("Total size: {} bytes", result.total_size);
        println!("Data length: {} bytes", result.data.len());

        assert_eq!(result.total_size as usize, result.data.len());
        println!("Download complete and verified!");

        Ok(())
    })
}
