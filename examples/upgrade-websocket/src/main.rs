use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    // Build an upgrade request (e.g., for WebSocket)
    let resp = client
        .get("wss://echo.websocket.org")?
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

        // Get the upgraded bidirectional IO stream
        let upgraded = resp.upgrade().await?;

        // `upgraded` implements hyper::rt::Read + hyper::rt::Write
        // and with the `tokio` feature, also tokio::io::AsyncRead + AsyncWrite.
        // You can pass it to a WebSocket library like tokio-tungstenite.

        println!("Got upgraded connection: {:?}", upgraded);
        println!("Use with tokio-tungstenite or another WebSocket library");
    } else {
        println!("Server did not upgrade: {}", resp.status());
        println!("Body: {}", resp.text().await?);
    }

    Ok(())
}
