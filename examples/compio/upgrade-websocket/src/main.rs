use aioduct::CompioClient;

fn main() -> Result<(), aioduct::Error> {
    compio_runtime::Runtime::new().unwrap().block_on(async {
        let client = CompioClient::builder().build_local().unwrap();

        let resp = client
            .get_local("wss://echo.websocket.org")?
            .upgrade()
            .header(
                http::header::HeaderName::from_static("sec-websocket-key"),
                http::HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
            )
            .header(
                http::header::HeaderName::from_static("sec-websocket-version"),
                http::HeaderValue::from_static("13"),
            )
            .send()
            .await?;

        println!("Status: {}", resp.status());

        if resp.status() == http::StatusCode::SWITCHING_PROTOCOLS {
            println!("Upgrade successful!");

            let upgraded = resp.upgrade().await?;

            println!("Got upgraded connection: {:?}", upgraded);
            println!("Use with a WebSocket library");
        } else {
            println!("Server did not upgrade: {}", resp.status());
            println!("Body: {}", resp.text().await?);
        }

        Ok(())
    })
}
