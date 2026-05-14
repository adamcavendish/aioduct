use aioduct::CompioClient;
use aioduct::runtime::compio_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        // Enable gzip, brotli, zstd, and deflate decompression
        // The client automatically sends Accept-Encoding and decompresses responses
        let client = CompioClient::builder_local(TcpConnector).build_local();

        // Request with gzip encoding
        let resp = client.get_local("https://httpbin.org/gzip")?.send().await?;

        println!("gzip — Status: {}", resp.status());
        println!(
            "Content-Encoding: {:?}",
            resp.headers().get("content-encoding")
        );
        let body = resp.text().await?;
        println!(
            "Body (auto-decompressed): {}...",
            &body[..body.len().min(200)]
        );

        // Request with deflate encoding
        let resp = client
            .get_local("https://httpbin.org/deflate")?
            .send()
            .await?;

        println!("\ndeflate — Status: {}", resp.status());
        let body = resp.text().await?;
        println!("Body: {}...", &body[..body.len().min(200)]);

        // You can also disable decompression by removing Accept-Encoding
        let resp = client
            .get_local("https://httpbin.org/gzip")?
            .header(
                http::header::ACCEPT_ENCODING,
                http::HeaderValue::from_static("identity"),
            )
            .send()
            .await?;

        println!(
            "\nNo decompression — raw bytes: {} bytes",
            resp.bytes().await?.len()
        );

        Ok(())
    })
}
