use aioduct::TokioClient;
use aioduct::runtime::tokio_rt::TcpConnector;
#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder(TcpConnector).build();

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
                match sse {
                    aioduct::SseEvent::Message(m) => {
                        println!(
                            "Event: type={:?}, data={:?}, id={:?}",
                            m.event, m.data, m.last_event_id
                        );
                        count += 1;
                    }
                    aioduct::SseEvent::Retry(ms) => {
                        println!("Retry: {ms}ms");
                    }
                }
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
