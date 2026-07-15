#![cfg(feature = "tokio")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use tokio::net::TcpListener;

use aioduct::HttpEngineSend;
use aioduct::runtime::TokioRuntime;
use aioduct::runtime::tokio_rt::TcpConnector;

use aioduct_test_server::h1::h1_server_with;

#[tokio::test]
async fn test_cache_stores_and_returns_fresh() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from("cached data")))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    // First request: hits the server, stores in cache
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached data");
    assert_eq!(attempt.load(Ordering::SeqCst), 1);

    // Second request: should be served from cache without hitting the server
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached data");
    assert_eq!(
        attempt.load(Ordering::SeqCst),
        1,
        "cache should prevent second server hit"
    );
}
#[cfg(feature = "gzip")]
#[tokio::test]
async fn test_cacheable_gzip_response_is_decompressed_before_return_and_cache_hit() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(b"cached gzip").unwrap();
    let compressed = Bytes::from(encoder.finish().unwrap());

    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();
    let compressed_clone = compressed.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        let compressed = compressed_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .header("content-encoding", "gzip")
                    .header("content-length", compressed.len().to_string())
                    .body(Full::new(compressed))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();
    let url = format!("http://{addr}/gzip-cache");

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert!(
        !resp.headers().contains_key("content-encoding"),
        "cacheable gzip response should expose decoded response headers"
    );
    assert_eq!(resp.text().await.unwrap(), "cached gzip");
    assert_eq!(attempt.load(Ordering::SeqCst), 1);

    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert!(
        !resp.headers().contains_key("content-encoding"),
        "cached gzip response should expose decoded response headers"
    );
    assert_eq!(resp.text().await.unwrap(), "cached gzip");
    assert_eq!(
        attempt.load(Ordering::SeqCst),
        1,
        "fresh cache hit must not contact the server again"
    );
}
#[tokio::test]
async fn test_cache_304_revalidation() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First request: return with ETag, short max-age
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, must-revalidate")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("original")))
                        .unwrap(),
                )
            } else {
                // Subsequent: check If-None-Match and return 304
                let inm = req
                    .headers()
                    .get("if-none-match")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                if inm.contains("\"v1\"") {
                    Ok(Response::builder()
                        .status(304)
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::new()))
                        .unwrap())
                } else {
                    Ok(Response::new(Full::new(Bytes::from("new data"))))
                }
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    // First: populate cache
    let resp = client
        .get(&format!("http://{addr}/revalidate"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");

    // Second: should revalidate and get 304, return cached body
    let resp = client
        .get(&format!("http://{addr}/revalidate"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn configured_retry_preserves_cache_entry_for_304_revalidation() {
    let attempts = Arc::new(AtomicU32::new(0));
    let server_attempts = attempts.clone();
    let (addr, _counter) = h1_server_with(move |request| {
        let attempts = server_attempts.clone();
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(match attempt {
                0 => Response::builder()
                    .header("cache-control", "max-age=0, must-revalidate")
                    .header("etag", "\"retry-v1\"")
                    .body(Full::new(Bytes::from_static(b"cached across retry")))
                    .unwrap(),
                1 => {
                    assert_eq!(request.headers()["if-none-match"], "\"retry-v1\"");
                    Response::builder()
                        .status(429)
                        .header("retry-after", "0")
                        .body(Full::new(Bytes::new()))
                        .unwrap()
                }
                _ => {
                    assert_eq!(request.headers()["if-none-match"], "\"retry-v1\"");
                    Response::builder()
                        .status(304)
                        .header("etag", "\"retry-v1\"")
                        .body(Full::new(Bytes::new()))
                        .unwrap()
                }
            })
        }
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(aioduct::HttpCache::new())
        .build()
        .unwrap();
    let url = format!("http://{addr}/retry-revalidation");

    assert_eq!(
        client
            .get(&url)
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "cached across retry"
    );
    let response = client
        .get(&url)
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(std::time::Duration::ZERO),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "cached across retry");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn configured_retry_preserves_stale_if_error_for_transport_failure() {
    let (origin_addr, _counter) = h1_server_with(|_request| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=0, stale-if-error=3600")
                .header("etag", "\"transport-v1\"")
                .body(Full::new(Bytes::from_static(b"stale after retry failure")))
                .unwrap(),
        )
    })
    .await;
    let retry_server_hits = Arc::new(AtomicU32::new(0));
    let retry_server_hits_clone = retry_server_hits.clone();
    let (retry_addr, _counter) = h1_server_with(move |request| {
        let hits = retry_server_hits_clone.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.headers()["if-none-match"], "\"transport-v1\"");
            Ok::<_, Infallible>(
                Response::builder()
                    .status(429)
                    .header("retry-after", "0")
                    .header("connection", "close")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
        }
    })
    .await;
    let dead_addr = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    };

    let cache = aioduct::HttpCache::new();
    let populate_client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache.clone())
        .resolver(move |_host: &str, _port: u16| {
            Box::pin(async move { Ok(origin_addr) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                >
        })
        .build()
        .unwrap();
    let url = format!(
        "http://cache-retry.test:{}/retry-stale-if-error#cache-fragment",
        origin_addr.port()
    );
    assert_eq!(
        populate_client
            .get(&url)
            .unwrap()
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "stale after retry failure"
    );
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let resolutions = Arc::new(AtomicU32::new(0));
    let resolver_calls = resolutions.clone();
    let retry_client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .resolver(move |_host: &str, _port: u16| {
            let attempt = resolver_calls.fetch_add(1, Ordering::SeqCst);
            let addr = if attempt == 0 { retry_addr } else { dead_addr };
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                >
        })
        .build()
        .unwrap();
    let response = retry_client
        .get(&url)
        .unwrap()
        .retry(
            aioduct::RetryConfig::default()
                .max_retries(1)
                .initial_backoff(std::time::Duration::ZERO),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(response.fragment(), Some("cache-fragment"));
    assert_eq!(response.text().await.unwrap(), "stale after retry failure");
    assert_eq!(retry_server_hits.load(Ordering::SeqCst), 1);
    assert!(resolutions.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn cache_last_modified_revalidation() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, must-revalidate")
                        .header("last-modified", "Sun, 06 Nov 1994 08:49:37 GMT")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("original")))
                        .unwrap(),
                )
            } else {
                let ims = req
                    .headers()
                    .get("if-modified-since")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                if ims.contains("Sun, 06 Nov 1994 08:49:37 GMT") {
                    Ok(Response::builder()
                        .status(304)
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::new()))
                        .unwrap())
                } else {
                    Ok(Response::new(Full::new(Bytes::from("new data"))))
                }
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/lm-reval"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");

    let resp = client
        .get(&format!("http://{addr}/lm-reval"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn cache_stale_if_error_expired() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, stale-if-error=1")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("cached")))
                        .unwrap(),
                )
            } else {
                let has_inm = req.headers().contains_key("if-none-match");
                assert!(has_inm, "revalidation should send If-None-Match");
                Ok(Response::builder()
                    .status(500)
                    .body(Full::new(Bytes::from("server error")))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/sie-expired"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");

    // Wait past the stale-if-error window (1 second)
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let resp = client
        .get(&format!("http://{addr}/sie-expired"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        http::StatusCode::INTERNAL_SERVER_ERROR,
        "500 should be returned when stale-if-error window has expired"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn cache_stale_if_error_non_5xx_is_not_served() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, stale-if-error=60")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("cached")))
                        .unwrap(),
                )
            } else {
                let has_inm = req.headers().contains_key("if-none-match");
                assert!(has_inm, "revalidation should send If-None-Match");
                Ok(Response::builder()
                    .status(404)
                    .body(Full::new(Bytes::from("not found")))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/sie-non-5xx"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");

    let resp = client
        .get(&format!("http://{addr}/sie-non-5xx"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        http::StatusCode::NOT_FOUND,
        "stale-if-error should only apply to 5xx, not 404"
    );
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_cache_stale_if_error_serves_stale_on_5xx() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, stale-if-error=3600")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("cached")))
                        .unwrap(),
                )
            } else {
                let has_inm = req.headers().contains_key("if-none-match");
                assert!(has_inm, "revalidation should send If-None-Match");
                Ok(Response::builder()
                    .status(500)
                    .body(Full::new(Bytes::from("server error")))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/sie"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");

    let resp = client
        .get(&format!("http://{addr}/sie"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "cached");
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_cache_stale_if_error_serves_stale_on_connection_error() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=0, stale-if-error=3600")
                    .header("etag", "\"v1\"")
                    .body(Full::new(Bytes::from("cached")))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache.clone())
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/sie"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");

    // Build a new client pointing at a dead port but using the same cache
    let dead_port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };
    let client2 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .resolver(move |_host: &str, _port: u16| {
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], dead_port));
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                >
        })
        .build()
        .unwrap();

    let resp = client2
        .get(&format!("http://{addr}/sie"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "cached");
}
#[tokio::test]
async fn test_cache_stale_if_error_not_applied_without_directive() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("cached")))
                        .unwrap(),
                )
            } else {
                Ok(Response::builder()
                    .status(500)
                    .body(Full::new(Bytes::from("server error")))
                    .unwrap())
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/no-sie"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");

    let resp = client
        .get(&format!("http://{addr}/no-sie"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
}
#[tokio::test]
async fn test_custom_cache_store_with_client() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingStore {
        inner: aioduct::InMemoryCacheStore,
        get_count: Arc<AtomicUsize>,
        put_count: Arc<AtomicUsize>,
    }

    impl aioduct::CacheStore for CountingStore {
        fn get(&self, method: &http::Method, uri: &http::Uri) -> Vec<aioduct::CacheEntry> {
            self.get_count.fetch_add(1, Ordering::Relaxed);
            self.inner.get(method, uri)
        }
        fn put(&self, method: &http::Method, uri: &http::Uri, entry: aioduct::CacheEntry) {
            self.put_count.fetch_add(1, Ordering::Relaxed);
            self.inner.put(method, uri, entry);
        }
        fn remove(&self, method: &http::Method, uri: &http::Uri) {
            self.inner.remove(method, uri);
        }
        fn clear(&self) {
            self.inner.clear();
        }
        fn len(&self) -> usize {
            self.inner.len()
        }
    }

    let get_count = Arc::new(AtomicUsize::new(0));
    let put_count = Arc::new(AtomicUsize::new(0));
    let store = CountingStore {
        inner: aioduct::InMemoryCacheStore::new(256),
        get_count: get_count.clone(),
        put_count: put_count.clone(),
    };
    let cache = aioduct::HttpCache::with_store(store);

    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=3600")
                .body(Full::new(Bytes::from("custom-cached")))
                .unwrap(),
        )
    })
    .await;

    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/custom"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "custom-cached");
    assert_eq!(put_count.load(Ordering::Relaxed), 1, "first request stores");

    let resp = client
        .get(&format!("http://{addr}/custom"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "custom-cached");
    assert!(
        get_count.load(Ordering::Relaxed) >= 2,
        "second request should hit store.get"
    );
    assert_eq!(
        put_count.load(Ordering::Relaxed),
        1,
        "second request should not store again"
    );
}
#[tokio::test]
async fn test_custom_cache_store_304_revalidation() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, must-revalidate")
                        .header("etag", "\"cs-v1\"")
                        .body(Full::new(Bytes::from("original")))
                        .unwrap(),
                )
            } else {
                let inm = req
                    .headers()
                    .get("if-none-match")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                if inm.contains("\"cs-v1\"") {
                    Ok(Response::builder()
                        .status(304)
                        .header("etag", "\"cs-v1\"")
                        .body(Full::new(Bytes::new()))
                        .unwrap())
                } else {
                    Ok(Response::new(Full::new(Bytes::from("new data"))))
                }
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::with_store(aioduct::InMemoryCacheStore::new(64));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/cs-reval"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");

    let resp = client
        .get(&format!("http://{addr}/cs-reval"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");
    assert_eq!(attempt.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn test_custom_cache_store_invalidation_on_post() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from(format!("v{n}"))))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::with_store(aioduct::InMemoryCacheStore::new(64));
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://{addr}/inv"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "v0");

    // Second GET: from cache
    let resp = client
        .get(&format!("http://{addr}/inv"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "v0");
    assert_eq!(attempt.load(Ordering::SeqCst), 1);

    // POST invalidates cache
    let _ = client
        .post(&format!("http://{addr}/inv"))
        .unwrap()
        .body("x")
        .send()
        .await
        .unwrap();

    // GET after POST: should hit server again
    let resp = client
        .get(&format!("http://{addr}/inv"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "v2");
}
#[tokio::test]
async fn test_custom_cache_store_shared_across_cloned_clients() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |_req| {
        let attempt = attempt_clone.clone();
        async move {
            attempt.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .body(Full::new(Bytes::from("shared")))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::HttpCache::with_store(aioduct::InMemoryCacheStore::new(64));
    let client1 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache.clone())
        .build()
        .unwrap();
    let client2 = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .build()
        .unwrap();

    let resp = client1
        .get(&format!("http://{addr}/shared"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "shared");
    assert_eq!(attempt.load(Ordering::SeqCst), 1);

    // client2 uses the same cache store — should get cache hit
    let resp = client2
        .get(&format!("http://{addr}/shared"))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "shared");
    assert_eq!(
        attempt.load(Ordering::SeqCst),
        1,
        "second client should use shared cache"
    );
}

// ── Bug-Finding Tests ─────────────────────────────────────────────────

// BUG: cache.rs completely ignores the Vary header. Two requests with different
// Accept-Encoding values should produce different cache entries, but they don't.
#[tokio::test]
async fn cache_should_respect_vary_header() {
    use std::time::Duration;

    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            let accept = req
                .headers()
                .get("accept-encoding")
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_else(|| "none".to_string());

            let body = format!("request={n} accept={accept}");
            Ok::<_, Infallible>(
                Response::builder()
                    .header("cache-control", "max-age=3600")
                    .header("vary", "Accept-Encoding")
                    .body(Full::new(Bytes::from(body)))
                    .unwrap(),
            )
        }
    })
    .await;

    let cache = aioduct::cache::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // First request: Accept-Encoding: gzip
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .header(
            http::header::ACCEPT_ENCODING,
            http::header::HeaderValue::from_static("gzip"),
        )
        .send()
        .await
        .unwrap();
    let body1 = resp.text().await.unwrap();
    assert!(
        body1.contains("request=0"),
        "first request should hit server"
    );
    assert!(body1.contains("accept=gzip"), "server sees gzip");

    // Second request: Accept-Encoding: br (different value)
    // With Vary: Accept-Encoding, this should be a cache MISS and hit the server.
    let resp = client
        .get(&format!("http://{addr}/resource"))
        .unwrap()
        .header(
            http::header::ACCEPT_ENCODING,
            http::header::HeaderValue::from_static("br"),
        )
        .send()
        .await
        .unwrap();
    let body2 = resp.text().await.unwrap();

    assert!(
        body2.contains("accept=br"),
        "BUG: cache.rs ignores the Vary header entirely. \
         Second request with Accept-Encoding: br got a cached response intended for gzip. \
         Got: {body2}"
    );
}

// #106: 304 revalidation should store cookies from the 304 response
#[tokio::test]
async fn cache_304_revalidation_stores_cookies() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let (addr, _counter) = h1_server_with(move |req| {
        let attempt = attempt_clone.clone();
        async move {
            let n = attempt.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("cache-control", "max-age=0, must-revalidate")
                        .header("etag", "\"v1\"")
                        .body(Full::new(Bytes::from("original")))
                        .unwrap(),
                )
            } else if n == 1 {
                let inm = req
                    .headers()
                    .get("if-none-match")
                    .map(|v| v.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                if inm.contains("\"v1\"") {
                    Ok(Response::builder()
                        .status(304)
                        .header("etag", "\"v1\"")
                        .header("set-cookie", "from304=yes; Path=/")
                        .body(Full::new(Bytes::new()))
                        .unwrap())
                } else {
                    Ok(Response::new(Full::new(Bytes::from("unexpected"))))
                }
            } else {
                let cookie = req
                    .headers()
                    .get("cookie")
                    .map(|v| v.to_str().unwrap().to_owned());
                let body = format!("cookie={}", cookie.unwrap_or_else(|| "none".into()));
                Ok(Response::new(Full::new(Bytes::from(body))))
            }
        }
    })
    .await;

    let cache = aioduct::HttpCache::new();
    let jar = aioduct::CookieJar::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .cookie_jar(jar)
        .build()
        .unwrap();

    let url = format!("http://{addr}/resource");

    // Populate cache
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");

    // Revalidation: server returns 304 with Set-Cookie
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "original");

    // Third request: cookie from 304 should be sent
    let resp = client.get(&url).unwrap().send().await.unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("from304=yes"),
        "cookie set during 304 revalidation should be stored and sent, got: {body}"
    );
}

// #107: fresh cache hit should apply middleware
#[tokio::test]
async fn fresh_cache_hit_applies_middleware() {
    use std::sync::atomic::AtomicBool;

    let (addr, _counter) = h1_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=3600")
                .body(Full::new(Bytes::from("cached")))
                .unwrap(),
        )
    })
    .await;

    let middleware_called = Arc::new(AtomicBool::new(false));
    let middleware_called_clone = middleware_called.clone();

    struct CacheMiddleware {
        called: Arc<AtomicBool>,
    }

    impl aioduct::Middleware for CacheMiddleware {
        fn on_response(
            &self,
            response: &mut http::Response<aioduct::body::RequestBodySend>,
            _uri: &http::Uri,
        ) {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            response.headers_mut().insert(
                http::header::HeaderName::from_static("x-from-middleware"),
                http::header::HeaderValue::from_static("applied"),
            );
        }
    }

    let cache = aioduct::HttpCache::new();
    let client = HttpEngineSend::<TokioRuntime, TcpConnector>::builder()
        .cache(cache)
        .middleware(CacheMiddleware {
            called: middleware_called_clone,
        })
        .build()
        .unwrap();

    let url = format!("http://{addr}/resource");

    // First request: populate cache
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "cached");
    middleware_called.store(false, std::sync::atomic::Ordering::SeqCst);

    // Second request: fresh cache hit — middleware should still run
    let resp = client.get(&url).unwrap().send().await.unwrap();
    assert!(
        middleware_called.load(std::sync::atomic::Ordering::SeqCst),
        "middleware on_response should be called on fresh cache hits"
    );
    assert_eq!(
        resp.headers()
            .get("x-from-middleware")
            .unwrap()
            .to_str()
            .unwrap(),
        "applied",
        "middleware header should be present on fresh cache hit response"
    );
    assert_eq!(resp.text().await.unwrap(), "cached");
}
