use aioduct::CompioClient;
use aioduct::runtime::compio_rt::TcpConnector;
fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = CompioClient::builder_local(TcpConnector)
            .build_local()
            .unwrap();

        // POST a URL-encoded form body
        let resp = client
            .post_local("https://httpbin.org/post")?
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
