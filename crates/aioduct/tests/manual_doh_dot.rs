//! Manual integration tests for DoH/DoT — require network access.
//! Run: cargo test -p aioduct --features tokio,rustls,rustls-ring,doh,dot --test manual_doh_dot -- --ignored --nocapture

#![cfg(all(
    feature = "tokio",
    feature = "doh",
    feature = "dot",
    feature = "rustls"
))]

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

fn builder() -> aioduct::client::ClientBuilder<TokioRuntime> {
    HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .timeout(std::time::Duration::from_secs(15))
}

#[tokio::test]
#[ignore]
async fn doh_cloudflare_resolves_httpbin() {
    let client = builder()
        .dns_over_https("1.1.1.1".parse().unwrap(), "cloudflare-dns.com")
        .unwrap()
        .build();

    let resp = client
        .get("https://httpbin.org/get")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("httpbin.org"),
        "body: {}",
        &body[..200.min(body.len())]
    );
    println!("[PASS] DoH via Cloudflare (1.1.1.1) resolved httpbin.org successfully");
}

#[tokio::test]
#[ignore]
async fn doh_google_resolves_httpbin() {
    let client = builder()
        .dns_over_https("8.8.8.8".parse().unwrap(), "dns.google")
        .unwrap()
        .build();

    let resp = client
        .get("https://httpbin.org/get")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "status: {}", resp.status());
    println!("[PASS] DoH via Google (8.8.8.8) resolved httpbin.org successfully");
}

#[tokio::test]
#[ignore]
async fn dot_cloudflare_resolves_httpbin() {
    let client = builder()
        .dns_over_tls("1.1.1.1".parse().unwrap(), "cloudflare-dns.com")
        .unwrap()
        .build();

    let resp = client
        .get("https://httpbin.org/get")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "status: {}", resp.status());
    println!("[PASS] DoT via Cloudflare (1.1.1.1) resolved httpbin.org successfully");
}

#[tokio::test]
#[ignore]
async fn dot_google_resolves_httpbin() {
    let client = builder()
        .dns_over_tls("8.8.8.8".parse().unwrap(), "dns.google")
        .unwrap()
        .build();

    let resp = client
        .get("https://httpbin.org/get")
        .unwrap()
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "status: {}", resp.status());
    println!("[PASS] DoT via Google (8.8.8.8) resolved httpbin.org successfully");
}
