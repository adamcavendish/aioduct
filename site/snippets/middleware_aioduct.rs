// features: tokio,tower
// runtime: tokio
use aioduct::TokioClient;
use aioduct::middleware::Middleware;

struct LoggingMiddleware;

impl Middleware for LoggingMiddleware {
    fn on_request(&self, _req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri) {
        println!("→ {} {}", _req.method(), _uri);
    }

    fn on_response(&self, _resp: &mut http::Response<aioduct::body::RequestBodySend>, _uri: &http::Uri) {
        println!("← {}", _resp.status());
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = TokioClient::builder()
        .middleware(LoggingMiddleware)
        .build()?;

    let resp = client.get("https://httpbin.org/get")?.send().await?;
    println!("Done: {}", resp.status());
    Ok(())
}
