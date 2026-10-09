// features: tokio,tracing
// runtime: tokio
use aioduct::{ConnectionEvent, RequestEvent, RequestObserver, TokioClient};

struct Events;
impl RequestObserver for Events {
    fn on_event(&self, event: &RequestEvent) {
        println!("{} {}: {:?}", event.method, event.uri.host().unwrap_or(""), event.phase);
    }
    fn on_connection_event(&self, event: &ConnectionEvent) {
        println!("connection: {:?}", event.phase);
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder().request_observer(Events).build()?;
    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Status: {}", resp.status());
    let _body = resp.bytes().await?;
    Ok(())
}
