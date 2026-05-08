// features: tokio,tower
// runtime: tokio
use aioduct::Client;
use aioduct::runtime::TokioRuntime;
use aioduct::middleware::Middleware;

struct LoggingMiddleware;

impl Middleware for LoggingMiddleware {
    fn before(&self, req: &mut aioduct::request::RequestBuilder<TokioRuntime>) {
        println!("→ {} {}", req.method(), req.url());
    }

    fn after(&self, resp: &aioduct::Response) {
        println!("← {} ({}ms)", resp.status(), resp.timings().total().as_millis());
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = Client::<TokioRuntime>::builder()
        .middleware(LoggingMiddleware)
        .build();

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Done: {}", resp.status());
    Ok(())
}
