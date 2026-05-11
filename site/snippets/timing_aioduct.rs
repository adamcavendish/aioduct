// features: tokio,json
// runtime: tokio
use aioduct::HttpEngine;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::main]
async fn main() -> Result<(), aioduct::Error> {
    let client = HttpEngine::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build();

    let resp = client.get("https://httpbin.org/get")?.send().await?;

    // Per-request timing breakdown
    let timings = resp.timings();
    println!("DNS:     {:?}", timings.dns());
    println!("TCP:     {:?}", timings.tcp());
    println!("TLS:     {:?}", timings.tls());
    println!("TTFB:    {:?}", timings.ttfb());
    println!("Total:   {:?}", timings.total());
    println!("Status:  {}", resp.status());
    Ok(())
}
