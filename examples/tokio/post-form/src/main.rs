use aioduct::TokioClient;
#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder().build().unwrap();

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
}
