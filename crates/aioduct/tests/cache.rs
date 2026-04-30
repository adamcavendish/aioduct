#![cfg(feature = "tokio")]

mod common;
use common::*;

#[tokio::test]
async fn test_cache_stores_and_returns_fresh() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |_req| {
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
    let client = Client::<TokioRuntime>::builder().cache(cache).build();

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

    let addr = start_server_with(move |_req| {
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
    let client = Client::<TokioRuntime>::builder().cache(cache).build();
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

    let addr = start_server_with(move |req| {
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
    let client = Client::<TokioRuntime>::builder().cache(cache).build();

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
async fn test_cache_stale_if_error_serves_stale_on_5xx() {
    let attempt = Arc::new(AtomicU32::new(0));
    let attempt_clone = attempt.clone();

    let addr = start_server_with(move |req| {
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
    let client = Client::<TokioRuntime>::builder().cache(cache).build();

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

    let addr = start_server_with(move |_req| {
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
    let client = Client::<TokioRuntime>::builder()
        .cache(cache.clone())
        .build();

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
    let client2 = Client::<TokioRuntime>::builder()
        .cache(cache)
        .resolver(move |_host: &str, _port: u16| {
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], dead_port));
            Box::pin(async move { Ok(addr) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<SocketAddr>> + Send>,
                >
        })
        .build();

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

    let addr = start_server_with(move |_req| {
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
    let client = Client::<TokioRuntime>::builder().cache(cache).build();

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
        fn get(&self, method: &http::Method, uri: &http::Uri) -> Option<aioduct::CacheEntry> {
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

    let addr = start_server_with(|_req| async {
        Ok::<_, Infallible>(
            Response::builder()
                .header("cache-control", "max-age=3600")
                .body(Full::new(Bytes::from("custom-cached")))
                .unwrap(),
        )
    })
    .await;

    let client = Client::<TokioRuntime>::builder().cache(cache).build();

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

    let addr = start_server_with(move |req| {
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
    let client = Client::<TokioRuntime>::builder().cache(cache).build();

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

    let addr = start_server_with(move |_req| {
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
    let client = Client::<TokioRuntime>::builder().cache(cache).build();

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

    let addr = start_server_with(move |_req| {
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
    let client1 = Client::<TokioRuntime>::builder()
        .cache(cache.clone())
        .build();
    let client2 = Client::<TokioRuntime>::builder().cache(cache).build();

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
