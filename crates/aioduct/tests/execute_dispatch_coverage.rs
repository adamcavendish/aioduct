#![cfg(feature = "tokio")]

//! Integration tests targeting specific uncovered lines in:
//! - client/execute_send.rs (stale-if-error, digest retry, HSTS, finalize_response)
//! - client/execute_local.rs (mirrors execute_send)
//! - client/dispatch.rs (connection_protocol, fire_connection_metrics, checkin)
//! - client/dispatch_send.rs (stale retry, pool hit, H2 multiplex)

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 1. HSTS upgrade during execute loop
//    Exercises execute_send.rs:30 (maybe_upgrade_hsts on the original URI)
//    and execute.rs:22-36 (maybe_upgrade_hsts implementation).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn hsts_upgrade_prevents_http_request() {
    // Pre-populate HSTS store with a known host
    let store = aioduct::hsts::HstsStore::new();
    let mut sts_headers = http::HeaderMap::new();
    sts_headers.insert(
        http::header::HeaderName::from_static("strict-transport-security"),
        "max-age=31536000".parse().unwrap(),
    );
    store.store_from_response("hsts-host.example.com", &sts_headers);

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .hsts(store)
        .timeout(Duration::from_secs(2))
        .build();

    // Request to http://hsts-host.example.com should be upgraded to https://
    // which will fail because there's no TLS server, but the important thing
    // is that it does NOT hit port 80.
    let result = client
        .get("http://hsts-host.example.com/path")
        .unwrap()
        .send()
        .await;

    // The request should fail (can't connect to port 443 on a non-existent host)
    // but it should NOT be an HttpsOnly error - it should be a connection error
    // because HSTS upgraded the URI to https://.
    assert!(result.is_err());
    let err = result.unwrap_err();
    // Verify it's NOT HttpsOnly (HSTS upgrade happened, it just can't connect)
    assert!(
        !err.to_string().contains("HTTPS only"),
        "HSTS should have upgraded the URI, error should be connection-related, got: {err}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 2. HSTS stores response header during execute loop
//    Exercises execute_send.rs:177-182 (hsts.store_from_response).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(feature = "rustls")]
#[tokio::test]
async fn hsts_stores_sts_header_from_response() {
    install_crypto();

    let store = aioduct::hsts::HstsStore::new();

    // Verify the host is NOT in HSTS store initially
    assert!(
        !store.should_upgrade("127.0.0.1"),
        "HSTS store should not know about 127.0.0.1 initially"
    );

    let (addr, cert_der, _counter) =
        aioduct_test_server::tls::tls_server_with(&[b"http/1.1"], |_req| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("strict-transport-security", "max-age=31536000")
                    .body(Full::new(Bytes::from("secure response")))
                    .unwrap(),
            )
        })
        .await;

    let cert = aioduct::tls::Certificate::from_der(cert_der.to_vec());
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .add_root_certificates(&[cert])
        .danger_accept_invalid_hostnames(true)
        .hsts(store.clone())
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("https://127.0.0.1:{}/", addr.port()))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "secure response");

    // After making HTTPS request, HSTS store should record the host
    assert!(
        store.should_upgrade("127.0.0.1"),
        "HSTS store should record host from Strict-Transport-Security response header"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 3. Digest auth retry with explicit version
//    Exercises execute_send.rs:258-259 (version applied to retry request).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn digest_auth_retry_with_http_version() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="version-test", nonce="version123", qop="auth""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                let version = format!("{:?}", req.version());
                Ok(Response::new(Full::new(Bytes::from(format!(
                    "version={version} auth_present={}",
                    !auth.is_empty()
                )))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .digest_auth("user", "pass")
        .timeout(Duration::from_secs(5))
        .build();

    // Use version(HTTP_11) explicitly to exercise the version path in retry
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .version(http::Version::HTTP_11)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("auth_present=true"),
        "digest auth retry should include authorization, got: {body}"
    );
    // The retry should have used HTTP/1.1 version
    assert!(
        body.contains("version=HTTP/1.1"),
        "digest auth retry should preserve HTTP version, got: {body}"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 4. Digest auth retry with middleware applied
//    Exercises execute_send.rs:263-264 (middleware.apply_request on retry).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn digest_auth_retry_applies_middleware() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(401)
                        .header(
                            "www-authenticate",
                            r#"Digest realm="mw-test", nonce="mwnonce1", qop="auth""#,
                        )
                        .body(Full::new(Bytes::from("unauthorized")))
                        .unwrap(),
                )
            } else {
                let mw_header = req
                    .headers()
                    .get("x-middleware-retry")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                Ok(Response::new(Full::new(Bytes::from(format!(
                    "mw={mw_header} auth_present={}",
                    auth.starts_with("Digest ")
                )))))
            }
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .digest_auth("user", "pass")
        .middleware(
            |req: &mut http::Request<aioduct::body::RequestBodySend>, _uri: &http::Uri| {
                req.headers_mut().insert(
                    http::header::HeaderName::from_static("x-middleware-retry"),
                    http::header::HeaderValue::from_static("applied"),
                );
            },
        )
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("mw=applied"),
        "middleware should be applied on digest auth retry, got: {body}"
    );
    assert!(
        body.contains("auth_present=true"),
        "digest auth should be present on retry, got: {body}"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 5. Cache invalidation on non-GET methods
//    Exercises execute_send.rs:167-169 (cache.invalidate).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn cache_invalidated_by_post_request() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let count = hit_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            if method == "GET" {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=3600")
                        .body(Full::new(Bytes::from(format!("get-response-{n}"))))
                        .unwrap(),
                )
            } else {
                Ok(Response::builder()
                    .body(Full::new(Bytes::from(format!(
                        "post-response method={method} path={path}"
                    ))))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .timeout(Duration::from_secs(5))
        .build();

    // First GET: stores in cache
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "get-response-0");
    assert_eq!(hit_count.load(Ordering::SeqCst), 1);

    // Second GET: served from cache (no server hit)
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "get-response-0");
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        1,
        "cache should serve second GET"
    );

    // POST to same URL: should invalidate cache
    let resp = client
        .post(&format!("http://{addr}/resource"))
        .unwrap()
        .body("data")
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("post-response"),
        "POST should succeed, got: {body}"
    );

    // Third GET: cache was invalidated, should hit server again
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "get-response-2",
        "cache should be invalidated after POST"
    );
    assert_eq!(hit_count.load(Ordering::SeqCst), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 6. Connection pool reuse (hit path in dispatch_send.rs:101-167)
//    Exercises the pool checkout hit path and checkin.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn connection_pool_reuse_exercises_hit_path() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    // First request: opens connection (pool miss)
    let resp = client
        .get(&format!("http://{addr}/first"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Second request: should reuse connection (pool hit)
    let resp = client
        .get(&format!("http://{addr}/second"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Third request: should also reuse
    let resp = client
        .get(&format!("http://{addr}/third"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Should have 3 requests but only 1 connection
    assert_eq!(
        counter.connections(),
        1,
        "should reuse the same connection for all 3 requests"
    );
    assert_eq!(counter.requests(), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 7. no_connection_reuse forces new connections
//    Exercises the skip of pool checkout when no_connection_reuse is set.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn no_connection_reuse_opens_new_connection_each_time() {
    let (addr, counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .no_connection_reuse()
        .timeout(Duration::from_secs(5))
        .build();

    // Each request should open a new connection
    for _ in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    assert_eq!(
        counter.connections(),
        3,
        "no_connection_reuse should open a new connection each time"
    );
    assert_eq!(counter.requests(), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 8. H2 connection pool hit and multiplex
//    Exercises dispatch_send.rs with H2 connections (multiplex path).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn h2_connection_reuse_multiplexes() {
    let (addr, counter) = aioduct_test_server::h2::h2_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("h2-response"))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(5))
        .build();

    // Multiple sequential requests should all multiplex over the same connection
    for i in 0..3 {
        let resp = client
            .get(&format!("http://{addr}/req{i}"))
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "h2-response");
    }

    // H2 multiplexes all requests over a single connection
    assert_eq!(
        counter.connections(),
        1,
        "H2 should multiplex all requests over one connection"
    );
    assert_eq!(counter.requests(), 3);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 9. Rate limiter sleep path during execute
//    Exercises dispatch_send.rs:52-56 (rate limiter wait loop).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn rate_limiter_sleep_path_in_dispatch() {
    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("ok"))))
    })
    .await;

    // Set a very low rate limit so the second request must wait
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .rate_limiter(aioduct::RateLimiter::new(1, Duration::from_millis(100)))
        .timeout(Duration::from_secs(5))
        .build();

    let start = std::time::Instant::now();

    // First request: immediate
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    // Second request: must wait for rate limiter
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "ok");

    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(90),
        "rate limiter should introduce delay, elapsed: {elapsed:?}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 10. Stale-if-error: server error path with stale cache entry
//     Exercises execute_send.rs:113-130 (server returns 5xx, stale cache serves).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn stale_if_error_serves_stale_on_503() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: cacheable response with stale-if-error
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, stale-if-error=3600")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("fresh-data")))
                        .unwrap(),
                )
            } else {
                // Subsequent: verify revalidation header, return 503
                let has_inm = req.headers().contains_key("if-none-match");
                assert!(has_inm, "revalidation should send If-None-Match");
                Ok(Response::builder()
                    .status(503)
                    .body(Full::new(Bytes::from("service unavailable")))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .timeout(Duration::from_secs(5))
        .build();

    // First request: populate cache
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "fresh-data");

    // Second request: server returns 503, stale-if-error should serve cached data
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "stale-if-error should serve stale cache on 503"
    );
    assert_eq!(
        resp.text().await.unwrap(),
        "fresh-data",
        "stale-if-error should serve original cached body"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 11. Stale-if-error: connection error path with stale cache entry
//     Exercises execute_send.rs:133-140 (error with stale cache fallback).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn stale_if_error_serves_stale_on_connection_failure() {
    let (addr, _counter) = h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=0, stale-if-error=3600")
                .header("etag", "\"conn-v1\"")
                .body(Full::new(Bytes::from("originally-cached")))
                .unwrap(),
        )
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache.clone())
        .timeout(Duration::from_secs(5))
        .build();

    // Populate cache
    let resp = client
        .get(&format!("http://{addr}/stale-conn"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "originally-cached");

    // Build a new client pointing at a dead port but using the same cache
    let dead_port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let client2 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .timeout(Duration::from_secs(2))
        .resolver(move |_host: &str, _port: u16| {
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], dead_port));
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                >
        })
        .build();

    // Request to dead port: should serve stale cached data
    let resp = client2
        .get(&format!("http://{addr}/stale-conn"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "stale-if-error should serve cached data when connection fails"
    );
    assert_eq!(resp.text().await.unwrap(), "originally-cached");
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 12. Observer receives connection metrics on pool checkin
//     Exercises dispatch.rs:44-61 (fire_connection_metrics).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn observer_receives_connection_metrics() {
    use std::sync::Mutex;

    #[derive(Default, Clone)]
    struct MetricsObserver {
        conn_events: Arc<Mutex<Vec<String>>>,
    }

    impl aioduct::observer::RequestObserver for MetricsObserver {
        fn on_event(&self, _event: &aioduct::observer::RequestEvent) {}
        fn on_connection_event(&self, event: &aioduct::observer::ConnectionEvent) {
            let desc = format!("{:?}", event.phase);
            self.conn_events.lock().unwrap().push(desc);
        }
    }

    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("metrics-test")))
                .unwrap(),
        )
    })
    .await;

    let obs = MetricsObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let events = obs.conn_events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "observer should receive connection metrics events"
    );
    // Connection metrics should contain Metrics phase
    let has_metrics = events.iter().any(|e| e.contains("Metrics"));
    assert!(
        has_metrics,
        "connection events should include Metrics phase, got: {events:?}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 13. GET request with no body exercises None arm in execute
//     Exercises execute_send.rs:52-57 (None body → empty Full).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn get_request_with_no_body() {
    let (addr, _counter) = h1_server_with(|req| async move {
        use http_body_util::BodyExt;
        let method = req.method().to_string();
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "method={method} body_len={}",
            body_bytes.len()
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("method=GET"),
        "should be GET request, got: {body}"
    );
    assert!(
        body.contains("body_len=0"),
        "GET request should have empty body, got: {body}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 14. Streaming body exercises the Streaming arm in execute
//     Exercises execute_send.rs:51 (Streaming body path).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn streaming_body_exercises_streaming_arm() {
    use http_body_util::BodyExt;

    let (addr, _counter) = h1_server_with(|req| async move {
        use http_body_util::BodyExt;
        let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!(
            "received={}",
            String::from_utf8_lossy(&body_bytes)
        )))))
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    // Create a streaming body (not buffered)
    let stream_body: aioduct::body::RequestBodySend =
        http_body_util::Full::new(Bytes::from("stream-payload"))
            .map_err(|never| match never {})
            .boxed_unsync();

    let resp = client
        .post(&format!("http://{addr}/"))
        .unwrap()
        .body_stream(stream_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("received=stream-payload"),
        "streaming body should be sent correctly, got: {body}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 15. finalize_response caches a cacheable response
//     Exercises execute_send.rs:303-311 (cache.store path in finalize_response).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn finalize_response_stores_cacheable_response() {
    let hit_count = Arc::new(AtomicU32::new(0));
    let hit_count_clone = hit_count.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let count = hit_count_clone.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from(format!("cacheable-{n}"))))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cache(cache)
        .timeout(Duration::from_secs(5))
        .build();

    // First request: finalize_response should store the response
    let resp = client
        .get(&format!("http://{addr}/cached"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "cacheable-0");

    // Second request: should come from cache (no server hit)
    let resp = client
        .get(&format!("http://{addr}/cached"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        "cacheable-0",
        "second request should serve from cache"
    );
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        1,
        "server should only be hit once due to caching"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 16. Cookie jar stores cookies from response
//     Exercises execute_send.rs:171-175 (cookie jar store path).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn cookie_jar_stores_set_cookie_from_response() {
    let (addr, _counter) = h1_server_with(|req| async move {
        let cookie_header = req
            .headers()
            .get("cookie")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        if cookie_header.is_empty() {
            // First request: set a cookie
            Ok::<_, Infallible>(
                Response::builder()
                    .header("set-cookie", "session=abc123; Path=/")
                    .body(Full::new(Bytes::from("cookie-set")))
                    .unwrap(),
            )
        } else {
            // Subsequent requests: echo back the cookie
            Ok(Response::new(Full::new(Bytes::from(format!(
                "cookie={cookie_header}"
            )))))
        }
    })
    .await;

    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .cookie_jar(jar)
        .timeout(Duration::from_secs(5))
        .build();

    // First request: server sets a cookie
    let resp = client
        .get(&format!("http://{addr}/page"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "cookie-set");

    // Second request: cookie jar should send the cookie back
    let resp = client
        .get(&format!("http://{addr}/page"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("session=abc123"),
        "cookie jar should send stored cookie, got: {body}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 17. Dispatch: stale connection retry path
//     Exercises dispatch_send.rs:169-213 (stale connection error → retry on fresh).
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn stale_connection_retry_succeeds_on_fresh_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request_count = Arc::new(AtomicU32::new(0));
    let request_count2 = request_count.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let count = request_count2.clone();

            tokio::spawn(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);

                if n == 0 {
                    // First connection: serve one response with keep-alive,
                    // then RST on next request.
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nfirst";
                    let _ = stream.write_all(response).await;
                    let _ = stream.flush().await;

                    // Wait for second request to arrive, then RST
                    let mut peek = [0u8; 1];
                    match stream.read(&mut peek).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    // RST the connection
                    let raw = stream.into_std().unwrap();
                    let sock = socket2::SockRef::from(&raw);
                    let _ = sock.set_linger(Some(Duration::from_secs(0)));
                    drop(raw);
                } else {
                    // Subsequent connections: serve normally
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response =
                        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nretry!";
                    let _ = stream.write_all(response).await;
                    let _ = stream.flush().await;
                }
            });
        }
    });

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .timeout(Duration::from_secs(5))
        .build();

    // First request: establishes connection, gets pooled
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "first");

    // Second request: stale connection is detected and retried on fresh connection
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        "retry!",
        "stale connection should be transparently retried"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 18. Observer events during pool hit vs miss
//     Exercises dispatch_send.rs observer notifications for pool outcomes.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[tokio::test]
async fn observer_reports_pool_hit_and_miss() {
    use std::sync::Mutex;

    #[derive(Default, Clone)]
    struct PoolObserver {
        phases: Arc<Mutex<Vec<String>>>,
    }

    impl aioduct::observer::RequestObserver for PoolObserver {
        fn on_event(&self, event: &aioduct::observer::RequestEvent) {
            let name = match &event.phase {
                aioduct::observer::RequestPhase::PoolCheckoutComplete { outcome, .. } => {
                    format!("PoolCheckout:{outcome:?}")
                }
                _ => return,
            };
            self.phases.lock().unwrap().push(name);
        }
        fn on_connection_event(&self, _event: &aioduct::observer::ConnectionEvent) {}
    }

    let (addr, _counter) = aioduct_test_server::h1::h1_server_with(|_req| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("connection", "keep-alive")
                .body(Full::new(Bytes::from("ok")))
                .unwrap(),
        )
    })
    .await;

    let obs = PoolObserver::default();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder(TcpConnector)
        .request_observer(obs.clone())
        .timeout(Duration::from_secs(5))
        .build();

    // First request: pool miss
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    // Second request: pool hit
    let resp = client
        .get(&format!("http://{addr}/"))
        .unwrap()
        .send()
        .await
        .unwrap();
    let _ = resp.bytes().await.unwrap();

    let phases = obs.phases.lock().unwrap();
    let has_miss = phases.iter().any(|p| p.contains("Miss"));
    let has_hit = phases.iter().any(|p| p.contains("Hit"));
    assert!(
        has_miss,
        "first request should report pool Miss, got: {phases:?}"
    );
    assert!(
        has_hit,
        "second request should report pool Hit, got: {phases:?}"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helper: install crypto provider for rustls tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

#[cfg(feature = "rustls")]
fn install_crypto() {
    aioduct_test_server::tls::install_crypto_provider();
}
