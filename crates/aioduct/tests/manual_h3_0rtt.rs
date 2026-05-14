//! Manual integration test for HTTP/3 0-RTT reconnection latency.
//! Run: cargo test -p aioduct --features tokio,rustls,rustls-ring,http3,quinn/runtime-tokio --test manual_h3_0rtt -- --ignored --nocapture

#![cfg(all(feature = "tokio", feature = "http3", feature = "rustls"))]

use std::time::Instant;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn h3_0rtt_reconnection_latency() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .http3(true)
        .unwrap()
        .h3_zero_rtt(true)
        .timeout(std::time::Duration::from_secs(15))
        .build();

    // First request: full handshake (stores session ticket)
    let t0 = Instant::now();
    let resp = client
        .get("https://cloudflare.com/")
        .unwrap()
        .send()
        .await
        .unwrap();
    let first_latency = t0.elapsed();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "first request failed: {}",
        resp.status()
    );
    let _ = resp.bytes().await;

    println!("[INFO] First request (full handshake): {:?}", first_latency);

    // Second request: should use 0-RTT if session ticket was cached
    let t1 = Instant::now();
    let resp = client
        .get("https://cloudflare.com/")
        .unwrap()
        .send()
        .await
        .unwrap();
    let second_latency = t1.elapsed();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "second request failed: {}",
        resp.status()
    );
    let _ = resp.bytes().await;

    println!(
        "[INFO] Second request (potential 0-RTT): {:?}",
        second_latency
    );
    println!(
        "[INFO] Speedup: {:.1}x",
        first_latency.as_secs_f64() / second_latency.as_secs_f64()
    );

    // Third request for good measure
    let t2 = Instant::now();
    let resp = client
        .get("https://cloudflare.com/")
        .unwrap()
        .send()
        .await
        .unwrap();
    let third_latency = t2.elapsed();
    assert!(
        resp.status().is_success() || resp.status().is_redirection(),
        "third request failed: {}",
        resp.status()
    );
    let _ = resp.bytes().await;

    println!(
        "[INFO] Third request (0-RTT/pool reuse): {:?}",
        third_latency
    );
    println!("[PASS] HTTP/3 0-RTT test completed — all requests succeeded");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn h3_without_0rtt_baseline() {
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .tls(aioduct::tls::RustlsConnector::with_webpki_roots())
        .http3(true)
        .unwrap()
        .h3_zero_rtt(false)
        .timeout(std::time::Duration::from_secs(15))
        .build();

    let t0 = Instant::now();
    let resp = client
        .get("https://cloudflare.com/")
        .unwrap()
        .send()
        .await
        .unwrap();
    let first_latency = t0.elapsed();
    assert!(resp.status().is_success() || resp.status().is_redirection());
    let _ = resp.bytes().await;

    let t1 = Instant::now();
    let resp = client
        .get("https://cloudflare.com/")
        .unwrap()
        .send()
        .await
        .unwrap();
    let second_latency = t1.elapsed();
    assert!(resp.status().is_success() || resp.status().is_redirection());
    let _ = resp.bytes().await;

    println!(
        "[INFO] Without 0-RTT — first: {:?}, second: {:?}",
        first_latency, second_latency
    );
    println!("[PASS] HTTP/3 baseline (no 0-RTT) completed");
}
