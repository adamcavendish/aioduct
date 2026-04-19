use aioduct::Client;
use aioduct::runtime::TokioRuntime;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder().build();

    // Connect to an SSE endpoint
    println!("Connecting to SSE stream...");
    let resp = client
        .get("https://httpbin.org/sse")?
        .header(
            http::header::ACCEPT,
            http::HeaderValue::from_static("text/event-stream"),
        )
        .send()
        .await?;

    println!("Status: {}", resp.status());

    // Convert to SSE stream and consume events
    let mut stream = resp.into_sse_stream();

    let mut count = 0;
    while let Some(event) = stream.next().await {
        match event {
            Ok(sse) => {
                println!(
                    "Event: type={:?}, data={:?}, id={:?}",
                    sse.event, sse.data, sse.id
                );
                count += 1;
                if count >= 5 {
                    println!("Received {count} events, stopping.");
                    break;
                }
            }
            Err(e) => {
                println!("SSE error: {e}");
                break;
            }
        }
    }

    Ok(())
}
