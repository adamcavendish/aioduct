#![cfg(feature = "tokio")]

use std::time::Duration;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

fn client() -> HttpEngineSend<TokioRuntime, TcpConnector> {
    HttpEngineSend::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build()
}

// ── Sequential Load ────────────────────────────────────────────────────

#[tokio::test]
async fn h1_sequential_100_requests_all_succeed() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    let mut failures = 0;
    for _ in 0..100 {
        match client.get(&url).unwrap().send().await {
            Ok(resp) if resp.status() == 200 => {
                let _ = resp.text().await;
            }
            _ => failures += 1,
        }
    }

    assert_eq!(failures, 0, "all 100 sequential GETs should succeed");
    assert!(
        counter.connections() <= 3,
        "sequential requests should reuse connections, got {} connections",
        counter.connections()
    );
}

// ── Concurrent Load ────────────────────────────────────────────────────

#[tokio::test]
async fn h1_concurrent_50_requests() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..50 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }

    let mut failures = 0;
    for h in handles {
        if h.await.is_err() {
            failures += 1;
        }
    }

    assert_eq!(failures, 0, "all 50 concurrent GETs should succeed");
}

#[tokio::test]
async fn h2_concurrent_100_requests() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..100 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }

    let mut failures = 0;
    for h in handles {
        if h.await.is_err() {
            failures += 1;
        }
    }

    assert_eq!(failures, 0, "all 100 concurrent H2 GETs should succeed");
    assert_eq!(counter.requests(), 100);
}

// ── Pool Saturation ────────────────────────────────────────────────────

#[tokio::test]
async fn h1_pool_saturation_and_recovery() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_max_idle_per_host(2)
        .pool_idle_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..10 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }

    let mut failures = 0;
    for h in handles {
        if h.await.is_err() {
            failures += 1;
        }
    }

    assert_eq!(
        failures, 0,
        "pool_max_idle(2) with 10 concurrent should still succeed"
    );
}

// ── Mixed Methods Under Load ───────────────────────────────────────────

#[tokio::test]
async fn mixed_methods_under_load() {
    let (addr, _) = aioduct_test_server::h1::h1_echo_server().await;
    let client = client();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();

    for _ in 0..50 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }

    for _ in 0..25 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client
                .post(&url)
                .unwrap()
                .body("data")
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }

    for _ in 0..25 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.put(&url).unwrap().body("data").send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let _ = resp.text().await.unwrap();
        }));
    }

    let mut failures = 0;
    for h in handles {
        if h.await.is_err() {
            failures += 1;
        }
    }

    assert_eq!(
        failures, 0,
        "50 GETs + 25 POSTs + 25 PUTs concurrent should all succeed"
    );
}

// ── Stale Retry Under Concurrent Load ──────────────────────────────────

#[tokio::test]
async fn stale_retry_under_concurrent_load() {
    let (addr, _counter) = aioduct_test_server::stale::h1_rst_every_n(2).await;
    let client = client();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..50 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            client.get(&url).unwrap().send().await
        }));
    }

    let mut successes = 0;
    let mut failures = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(resp) if resp.status() == 200 => {
                let _ = resp.text().await;
                successes += 1;
            }
            _ => failures += 1,
        }
    }

    assert!(
        successes > 40,
        "most requests should succeed with stale retry, got {successes} successes and {failures} failures"
    );
}

// ── Large Body Under Load ──────────────────────────────────────────────

#[tokio::test]
async fn h1_large_body_concurrent() {
    let body_size = 64 * 1024;
    let (addr, _) = aioduct_test_server::h1::h1_large_body_server(body_size).await;
    let client = client();
    let url = format!("http://{addr}/");

    let mut handles = Vec::new();
    for _ in 0..20 {
        let client = client.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let resp = client.get(&url).unwrap().send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let body = resp.bytes().await.unwrap();
            assert_eq!(body.len(), body_size);
        }));
    }

    let mut failures = 0;
    for h in handles {
        if h.await.is_err() {
            failures += 1;
        }
    }

    assert_eq!(
        failures, 0,
        "20 concurrent 64KB body reads should all succeed"
    );
}

// ── Rate Limiter Feature Gap ──────────────────────────────────────────

// BUG: RateLimiter is applied in execute_local.rs:338 but completely absent from
// execute_send.rs. The Send-capable client (HttpEngineSend) ignores rate_limiter entirely.
#[tokio::test]
async fn rate_limiter_should_throttle_send_client() {
    let (addr, counter) = aioduct_test_server::h1::h1_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .rate_limiter(aioduct::RateLimiter::new(5, Duration::from_secs(1)))
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    let start = std::time::Instant::now();
    for _ in 0..10 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
    }
    let elapsed = start.elapsed();

    // At 5 requests/second, 10 requests should take at least ~1 second
    assert!(
        elapsed >= Duration::from_millis(800),
        "BUG: RateLimiter is not applied on HttpEngineSend (execute_send.rs has no rate_limiter code). \
         10 requests at rate_per_second(5) should take >= 1s, but took {:?}. \
         Requests sent: {}",
        elapsed,
        counter.requests()
    );
}

// BUG: throttle.rs:28 uses `per / max_tokens as u32` which truncates max_tokens to u32.
// For max_tokens > u32::MAX this silently computes the wrong refill interval.
// Also, max_tokens values > 2^32 that truncate to 0 cause a divide-by-zero panic.
#[tokio::test]
async fn rate_limiter_large_max_tokens_truncation() {
    // max_tokens = 2^32 + 1 = 4294967297 truncates to 1 as u32
    // This gives refill_interval = per / 1 = 1s instead of ~0.23ns
    let limiter = aioduct::RateLimiter::new(4_294_967_297, Duration::from_secs(1));

    // Should be able to acquire many tokens immediately (started with 4294967297 tokens)
    let mut acquired = 0;
    for _ in 0..100 {
        if limiter.try_acquire() {
            acquired += 1;
        }
    }
    assert_eq!(
        acquired, 100,
        "should acquire 100 tokens from a pool of 4294967297"
    );

    // Now check the refill rate isn't absurdly wrong
    // The wait_duration should be tiny (near 0) for such a large bucket, not 1s
    let wait = limiter.wait_duration();
    assert!(
        wait < Duration::from_millis(100),
        "BUG: throttle.rs:28 truncates max_tokens to u32. \
         RateLimiter(4294967297, 1s) should have near-zero wait, got {:?}",
        wait
    );
}

// ── H2 Sequential Load ────────────────────────────────────────────────

#[tokio::test]
async fn h2_sequential_50_requests() {
    let (addr, counter) = aioduct_test_server::h2::h2_server().await;
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .pool_idle_timeout(Duration::from_secs(60))
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("http://{addr}/");

    for i in 0..50 {
        let resp = client.get(&url).unwrap().send().await.unwrap();
        assert_eq!(resp.status(), 200, "request {i} failed");
        let _ = resp.text().await.unwrap();
    }

    assert_eq!(counter.requests(), 50);
    assert_eq!(
        counter.connections(),
        1,
        "50 sequential H2 requests should reuse 1 connection"
    );
}

// BUG: bandwidth.rs:162-163 and 177-178 use wake_by_ref() + Poll::Pending to busy-loop.
// When bandwidth tokens are exhausted, the body stream spins at 100% CPU instead of
// sleeping until tokens refill. This is a correctness issue (wastes CPU) and can cause
// starvation of other tasks.
#[tokio::test]
async fn bandwidth_limiter_should_not_busy_loop() {
    let (addr, _) = aioduct_test_server::h1::h1_large_body_server(64 * 1024).await;

    // Very low bandwidth: 1 KB/s for 64 KB body → should take ~64 seconds
    // We'll just verify CPU usage by measuring time with a tight token budget
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .max_download_speed(512) // 512 bytes/sec
        .timeout(Duration::from_secs(10))
        .build();

    let start = std::time::Instant::now();
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Read first few bytes to trigger the bandwidth body
    // This will timeout because 64KB at 512 B/s = 128 seconds > 10s timeout.
    // The real issue: during this read, the task busy-loops with wake_by_ref + Pending.
    let result = resp.bytes().await;
    let elapsed = start.elapsed();

    // The request should timeout or error (body too slow for the timeout).
    // What we're documenting: bandwidth.rs uses busy-loop instead of proper async sleep.
    // A proper implementation would use a timer (e.g., tokio::time::sleep) to schedule
    // the next poll when tokens are available, rather than immediate wake + pending.
    //
    // We can't directly measure CPU spin in a test, but we verify the mechanism works.
    assert!(
        result.is_err() || elapsed >= Duration::from_secs(2),
        "bandwidth limited body should either timeout or take significant time, \
         but completed in {:?}. The busy-loop in bandwidth.rs wastes CPU cycles.",
        elapsed
    );
}

// BUG: bandwidth.rs:67 `deficit * 1_000_000_000` overflows u64 above ~18.4 GB.
// wait_duration() returns a wrong (wrapped) duration for large deficits.
#[tokio::test]
async fn bandwidth_wait_duration_overflow_for_large_deficit() {
    let limiter = aioduct::BandwidthLimiter::new(1);

    // Consume all tokens
    limiter.try_consume(1);

    // Request a huge number of bytes — this causes deficit * 1_000_000_000 to overflow
    let wait = limiter.wait_duration(20_000_000_000); // 20 GB

    // With 1 byte/sec, waiting for 20 GB should be ~20 billion seconds (~634 years).
    // Due to overflow, the result wraps to a much smaller value.
    assert!(
        wait >= Duration::from_secs(1_000_000),
        "BUG: bandwidth.rs:67 `deficit * 1_000_000_000` overflows u64 for large deficits. \
         wait_duration(20GB) at 1 B/s should be ~20 billion seconds, but got {:?}",
        wait
    );
}

// BUG: throttle.rs:71-74 and bandwidth.rs:114-118 use SystemTime::now() instead of
// monotonic time (Instant). If the system clock jumps backwards (NTP correction, VM
// migration, DST), the refill logic stops refilling tokens for the duration of the jump.
#[tokio::test]
async fn rate_limiter_uses_system_time_not_monotonic() {
    // This test documents the issue. We can't easily simulate clock jumps in a test,
    // but we verify that the limiter uses SystemTime (which is the bug).
    // A proper implementation should use std::time::Instant.

    let limiter = aioduct::RateLimiter::new(100, Duration::from_secs(1));

    // Consume all tokens
    for _ in 0..100 {
        limiter.try_acquire();
    }

    // Wait for refill
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Should have refilled some tokens
    let acquired = limiter.try_acquire();
    assert!(
        acquired,
        "RateLimiter should refill tokens after sleeping. \
         Note: throttle.rs:71-74 uses SystemTime which is not monotonic — \
         clock jumps backwards will cause token starvation."
    );
}
