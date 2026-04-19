use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aioduct::runtime::TokioRuntime;
use aioduct::{Client, Middleware};

struct LoggingMiddleware;

impl Middleware for LoggingMiddleware {
    fn on_request(&self, request: &mut http::Request<aioduct::HyperBody>, uri: &http::Uri) {
        println!("[MW] → {} {}", request.method(), uri);
    }

    fn on_response(&self, response: &mut http::Response<aioduct::HyperBody>, uri: &http::Uri) {
        println!("[MW] ← {} {}", response.status(), uri);
    }

    fn on_error(&self, error: &aioduct::Error, uri: &http::Uri, method: &http::Method) {
        println!("[MW] ! {method} {uri} failed: {error}");
    }

    fn on_redirect(&self, status: http::StatusCode, from: &http::Uri, to: &http::Uri) {
        println!("[MW] ↪ {status} {from} → {to}");
    }

    fn on_retry(
        &self,
        error: &aioduct::Error,
        uri: &http::Uri,
        method: &http::Method,
        attempt: u32,
    ) {
        println!("[MW] ↻ retry #{attempt} for {method} {uri}: {error}");
    }
}

#[derive(Clone)]
struct MetricsMiddleware {
    request_count: Arc<AtomicU64>,
}

impl Middleware for MetricsMiddleware {
    fn on_request(&self, _request: &mut http::Request<aioduct::HyperBody>, _uri: &http::Uri) {
        let count = self.request_count.fetch_add(1, Ordering::Relaxed) + 1;
        println!("[Metrics] request #{count}");
    }
}

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let request_count = Arc::new(AtomicU64::new(0));

    let metrics = MetricsMiddleware {
        request_count: request_count.clone(),
    };

    let client = Client::<TokioRuntime>::builder()
        .middleware(LoggingMiddleware)
        .middleware(metrics)
        .build();

    // Middleware sees each request/response
    let _resp = client.get("https://httpbin.org/get")?.send().await?;

    // Redirect — middleware sees redirect events
    let _resp = client.get("https://httpbin.org/redirect/2")?.send().await?;

    println!(
        "\nTotal requests: {}",
        request_count.load(Ordering::Relaxed)
    );

    // Closure as middleware (request-only)
    let client = Client::<TokioRuntime>::builder()
        .middleware(
            |req: &mut http::Request<aioduct::HyperBody>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    "x-injected",
                    http::HeaderValue::from_static("by-middleware"),
                );
            },
        )
        .build();

    let resp = client.get("https://httpbin.org/headers")?.send().await?;

    println!("\nHeaders with injected header:\n{}", resp.text().await?);

    Ok(())
}
