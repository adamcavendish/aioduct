use aioduct::CompioClient;
use aioduct::runtime::compio_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = CompioClient::builder_local(TcpConnector).build_local();

        // Simple GET request
        let resp = client.get_local("https://httpbin.org/get")?.send().await?;

        println!("Status: {}", resp.status());
        println!("URL: {}", resp.url());
        println!("Version: {:?}", resp.version());

        // Read response headers
        for (name, value) in resp.headers() {
            println!("  {name}: {}", value.to_str().unwrap_or("<binary>"));
        }

        // Read body as text
        let body = resp.text().await?;
        println!("\nBody:\n{body}");

        Ok(())
    })
}
