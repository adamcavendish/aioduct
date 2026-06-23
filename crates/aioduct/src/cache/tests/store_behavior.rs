use super::*;

#[test]
fn test_custom_store() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingStore {
        inner: InMemoryCacheStore,
        put_count: Arc<AtomicUsize>,
    }

    impl CacheStore for CountingStore {
        fn get(&self, method: &Method, uri: &Uri) -> Vec<CacheEntry> {
            self.inner.get(method, uri)
        }
        fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
            self.put_count.fetch_add(1, Ordering::Relaxed);
            self.inner.put(method, uri, entry);
        }
        fn remove(&self, method: &Method, uri: &Uri) {
            self.inner.remove(method, uri);
        }
        fn clear(&self) {
            self.inner.clear();
        }
        fn len(&self) -> usize {
            self.inner.len()
        }
    }

    let put_count = Arc::new(AtomicUsize::new(0));
    let store = CountingStore {
        inner: InMemoryCacheStore::new(256),
        put_count: put_count.clone(),
    };
    let cache = HttpCache::with_store(store);

    let uri: Uri = "http://example.com/custom".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("custom"),
        &HeaderMap::new(),
    );

    assert_eq!(cache.len(), 1);
    assert_eq!(put_count.load(Ordering::Relaxed), 1);

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.body, Bytes::from("custom"));
        }
        _ => panic!("expected fresh hit from custom store"),
    }
}

#[test]
fn test_in_memory_store_debug() {
    let store = InMemoryCacheStore::new(256);
    let dbg = format!("{store:?}");
    assert!(dbg.contains("InMemoryCacheStore"));
}

#[test]
fn test_in_memory_store_get_put_remove() {
    let store = InMemoryCacheStore::new(256);
    let uri: Uri = "http://example.com/a".parse().unwrap();

    assert!(store.get(&Method::GET, &uri).is_empty());
    assert!(store.is_empty());

    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("body"),
        stored_at: Instant::now(),
        max_age: Some(Duration::from_secs(60)),
        expires_at: None,
        etag: None,
        last_modified: None,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
        vary: None,
        request_vary_headers: None,
    };
    store.put(&Method::GET, &uri, entry);
    assert_eq!(store.len(), 1);
    assert!(!store.is_empty());

    let got = store.get(&Method::GET, &uri);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].body, Bytes::from("body"));

    store.remove(&Method::GET, &uri);
    assert!(store.get(&Method::GET, &uri).is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn test_in_memory_store_clear() {
    let store = InMemoryCacheStore::new(256);
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=60".parse().unwrap());

    let cache = HttpCache::with_store(store);
    for i in 0..5 {
        let uri: Uri = format!("http://example.com/{i}").parse().unwrap();
        cache.store(
            &Method::GET,
            &uri,
            StatusCode::OK,
            &headers,
            &Bytes::from("x"),
            &HeaderMap::new(),
        );
    }
    assert_eq!(cache.len(), 5);
    cache.clear();
    assert!(cache.is_empty());
}

#[test]
fn test_in_memory_store_eviction_oldest() {
    let store = InMemoryCacheStore::new(2);

    let entry = |body: &str| CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from(body.to_owned()),
        stored_at: Instant::now(),
        max_age: Some(Duration::from_secs(3600)),
        expires_at: None,
        etag: None,
        last_modified: None,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
        vary: None,
        request_vary_headers: None,
    };

    let uri_a: Uri = "http://example.com/a".parse().unwrap();
    let uri_b: Uri = "http://example.com/b".parse().unwrap();
    let uri_c: Uri = "http://example.com/c".parse().unwrap();

    store.put(&Method::GET, &uri_a, entry("a"));
    store.put(&Method::GET, &uri_b, entry("b"));
    assert_eq!(store.len(), 2);

    store.put(&Method::GET, &uri_c, entry("c"));
    assert_eq!(store.len(), 2);
    assert!(
        store.get(&Method::GET, &uri_a).is_empty(),
        "oldest entry (a) should be evicted"
    );
    assert!(!store.get(&Method::GET, &uri_b).is_empty());
    assert!(!store.get(&Method::GET, &uri_c).is_empty());
}

#[test]
fn test_in_memory_store_put_existing_key_no_eviction() {
    let store = InMemoryCacheStore::new(2);

    let entry = |body: &str| CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from(body.to_owned()),
        stored_at: Instant::now(),
        max_age: Some(Duration::from_secs(3600)),
        expires_at: None,
        etag: None,
        last_modified: None,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
        vary: None,
        request_vary_headers: None,
    };

    let uri_a: Uri = "http://example.com/a".parse().unwrap();
    let uri_b: Uri = "http://example.com/b".parse().unwrap();

    store.put(&Method::GET, &uri_a, entry("a1"));
    store.put(&Method::GET, &uri_b, entry("b1"));

    store.put(&Method::GET, &uri_a, entry("a2"));
    assert_eq!(store.len(), 2);
    let got = store.get(&Method::GET, &uri_a);
    assert_eq!(got[0].body, Bytes::from("a2"));
}

#[test]
fn test_in_memory_store_separate_method_keys() {
    let store = InMemoryCacheStore::new(256);

    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("get-body"),
        stored_at: Instant::now(),
        max_age: Some(Duration::from_secs(60)),
        expires_at: None,
        etag: None,
        last_modified: None,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
        vary: None,
        request_vary_headers: None,
    };
    let head_entry = CacheEntry {
        body: Bytes::from("head-body"),
        ..entry.clone()
    };

    let uri: Uri = "http://example.com/x".parse().unwrap();
    store.put(&Method::GET, &uri, entry);
    store.put(&Method::HEAD, &uri, head_entry);
    assert_eq!(store.len(), 2);

    let get_val = store.get(&Method::GET, &uri);
    assert_eq!(get_val[0].body, Bytes::from("get-body"));
    let head_val = store.get(&Method::HEAD, &uri);
    assert_eq!(head_val[0].body, Bytes::from("head-body"));

    store.remove(&Method::GET, &uri);
    assert!(store.get(&Method::GET, &uri).is_empty());
    assert!(!store.get(&Method::HEAD, &uri).is_empty());
}

#[test]
fn test_custom_store_invalidate_calls_remove() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TrackingStore {
        inner: InMemoryCacheStore,
        remove_count: Arc<AtomicUsize>,
    }

    impl CacheStore for TrackingStore {
        fn get(&self, method: &Method, uri: &Uri) -> Vec<CacheEntry> {
            self.inner.get(method, uri)
        }
        fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
            self.inner.put(method, uri, entry);
        }
        fn remove(&self, method: &Method, uri: &Uri) {
            self.remove_count.fetch_add(1, Ordering::Relaxed);
            self.inner.remove(method, uri);
        }
        fn clear(&self) {
            self.inner.clear();
        }
        fn len(&self) -> usize {
            self.inner.len()
        }
    }

    let remove_count = Arc::new(AtomicUsize::new(0));
    let store = TrackingStore {
        inner: InMemoryCacheStore::new(256),
        remove_count: remove_count.clone(),
    };
    let cache = HttpCache::with_store(store);

    let uri: Uri = "http://example.com/res".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("data"),
        &HeaderMap::new(),
    );

    cache.invalidate(&Method::POST, &uri);
    assert_eq!(
        remove_count.load(Ordering::Relaxed),
        2,
        "invalidate should call remove for GET and HEAD"
    );
}

#[test]
fn test_custom_store_clear_and_len() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FlagStore {
        inner: InMemoryCacheStore,
        cleared: Arc<AtomicBool>,
    }

    impl CacheStore for FlagStore {
        fn get(&self, method: &Method, uri: &Uri) -> Vec<CacheEntry> {
            self.inner.get(method, uri)
        }
        fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
            self.inner.put(method, uri, entry);
        }
        fn remove(&self, method: &Method, uri: &Uri) {
            self.inner.remove(method, uri);
        }
        fn clear(&self) {
            self.cleared.store(true, Ordering::Relaxed);
            self.inner.clear();
        }
        fn len(&self) -> usize {
            self.inner.len()
        }
    }

    let cleared = Arc::new(AtomicBool::new(false));
    let store = FlagStore {
        inner: InMemoryCacheStore::new(256),
        cleared: cleared.clone(),
    };
    let cache = HttpCache::with_store(store);

    let uri: Uri = "http://example.com/f".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );
    assert_eq!(cache.len(), 1);
    assert!(!cache.is_empty());

    cache.clear();
    assert!(cleared.load(Ordering::Relaxed));
    assert!(cache.is_empty());
}

#[test]
fn test_with_store_fresh_lookup_through_policy() {
    let store = InMemoryCacheStore::new(256);
    let cache = HttpCache::with_store(store);

    let uri: Uri = "http://example.com/ws".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("ws-data"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.status, StatusCode::OK);
            assert_eq!(resp.body, Bytes::from("ws-data"));
        }
        _ => panic!("expected fresh hit via with_store"),
    }
}

#[test]
fn test_with_store_stale_revalidation() {
    let store = InMemoryCacheStore::new(256);
    let cache = HttpCache::with_store(store);

    let uri: Uri = "http://example.com/stale".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=0".parse().unwrap());
    headers.insert(ETAG, "\"custom-v1\"".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("old"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { validators, .. } => {
            assert_eq!(validators.etag.as_deref(), Some("\"custom-v1\""));
        }
        _ => panic!("expected stale with validators via custom store"),
    }
}

#[test]
fn test_with_store_no_store_directive_skips_put() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingStore {
        inner: InMemoryCacheStore,
        put_count: Arc<AtomicUsize>,
    }

    impl CacheStore for CountingStore {
        fn get(&self, method: &Method, uri: &Uri) -> Vec<CacheEntry> {
            self.inner.get(method, uri)
        }
        fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
            self.put_count.fetch_add(1, Ordering::Relaxed);
            self.inner.put(method, uri, entry);
        }
        fn remove(&self, method: &Method, uri: &Uri) {
            self.inner.remove(method, uri);
        }
        fn clear(&self) {
            self.inner.clear();
        }
        fn len(&self) -> usize {
            self.inner.len()
        }
    }

    let put_count = Arc::new(AtomicUsize::new(0));
    let store = CountingStore {
        inner: InMemoryCacheStore::new(256),
        put_count: put_count.clone(),
    };
    let cache = HttpCache::with_store(store);

    let uri: Uri = "http://example.com/ns".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("secret"),
        &HeaderMap::new(),
    );

    assert_eq!(put_count.load(Ordering::Relaxed), 0);
    assert!(cache.is_empty());
}

#[test]
fn test_with_store_clone_shares_backend() {
    let cache = HttpCache::with_store(InMemoryCacheStore::new(256));
    let cache2 = cache.clone();

    let uri: Uri = "http://example.com/shared".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("shared"),
        &HeaderMap::new(),
    );

    assert_eq!(cache2.len(), 1);
    match cache2.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.body, Bytes::from("shared"));
        }
        _ => panic!("cloned cache should see entries from original"),
    }
}
