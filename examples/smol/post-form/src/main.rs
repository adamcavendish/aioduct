use aioduct::SmolClient;
use aioduct::runtime::smol_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    smol::block_on(async {
        let client = SmolClient::builder(TcpConnector).build();

        // POST a URL-encoded form body
        let resp = client
            .post("https://httpbin.org/post")?
            .form(&[("username", "alice"), ("password", "s3cret")])
            .send()
            .await?;

        println!("Status: {}", resp.status());
        println!("Content-Length: {:?}", resp.content_length());

        let body = resp.text().await?;
        println!("\nResponse:\n{body}");

        Ok(())
    })
}
