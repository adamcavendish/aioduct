use super::*;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use http::header::{CACHE_CONTROL, ETAG, EXPIRES, LAST_MODIFIED};
use http::{HeaderMap, Method, StatusCode, Uri};

#[path = "tests/expires_freshness.rs"]
mod expires_freshness;

#[test]
fn test_cache_store_and_fresh_lookup() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/test".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    let body = Bytes::from("hello");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.status, StatusCode::OK);
            assert_eq!(resp.body, Bytes::from("hello"));
        }
        _ => panic!("expected fresh cache hit"),
    }
}

#[test]
fn test_cache_no_store() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/secret".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    let body = Bytes::from("secret");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );
    assert!(cache.is_empty());
}

#[test]
fn test_cache_stale_with_etag() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/data".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=0".parse().unwrap());
    headers.insert(ETAG, "\"abc123\"".parse().unwrap());
    let body = Bytes::from("data");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { validators, .. } => {
            assert_eq!(validators.etag.as_deref(), Some("\"abc123\""));
        }
        _ => panic!("expected stale cache hit with validators"),
    }
}

#[test]
fn test_cache_post_not_cached() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/api".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    let body = Bytes::from("result");

    cache.store(
        &Method::POST,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );
    assert!(cache.is_empty());
}

#[test]
fn test_cache_invalidation_on_post() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/resource".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    let body = Bytes::from("data");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );
    assert_eq!(cache.len(), 1);

    cache.invalidate(&Method::POST, &uri);
    assert!(cache.is_empty());
}

#[test]
fn test_max_entries_eviction() {
    let cache = HttpCache::with_config(CacheConfig { max_entries: 2 });
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());

    for i in 0..3 {
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

    assert_eq!(cache.len(), 2);
}

#[test]
fn test_httpdate_parse() {
    let result = httpdate_parse("Sun, 06 Nov 1994 08:49:37 GMT");
    assert!(result.is_some());
}

#[test]
fn test_httpdate_parse_invalid() {
    assert!(httpdate_parse("not a date").is_none());
    assert!(httpdate_parse("").is_none());
}

#[test]
fn test_httpdate_parse_invalid_month() {
    assert!(httpdate_parse("Sun, 06 Foo 1994 08:49:37 GMT").is_none());
}

#[test]
fn test_httpdate_parse_invalid_time() {
    assert!(httpdate_parse("Sun, 06 Nov 1994 08:49 GMT").is_none());
}

#[test]
fn test_is_cacheable_status() {
    assert!(is_cacheable_status(StatusCode::OK));
    assert!(is_cacheable_status(StatusCode::NOT_FOUND));
    assert!(is_cacheable_status(StatusCode::MOVED_PERMANENTLY));
    assert!(!is_cacheable_status(StatusCode::UNAUTHORIZED));
    assert!(!is_cacheable_status(StatusCode::INTERNAL_SERVER_ERROR));
}

#[test]
fn test_is_cacheable_method() {
    assert!(is_cacheable_method(&Method::GET));
    assert!(is_cacheable_method(&Method::HEAD));
    assert!(!is_cacheable_method(&Method::POST));
    assert!(!is_cacheable_method(&Method::PUT));
    assert!(!is_cacheable_method(&Method::DELETE));
}

#[test]
fn test_is_unsafe_method() {
    assert!(!is_unsafe_method(&Method::GET));
    assert!(!is_unsafe_method(&Method::HEAD));
    assert!(!is_unsafe_method(&Method::OPTIONS));
    assert!(!is_unsafe_method(&Method::TRACE));
    assert!(is_unsafe_method(&Method::POST));
    assert!(is_unsafe_method(&Method::PUT));
    assert!(is_unsafe_method(&Method::DELETE));
    assert!(is_unsafe_method(&Method::PATCH));
}

#[test]
fn test_is_response_cacheable() {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=60".parse().unwrap());
    assert!(is_response_cacheable(StatusCode::OK, &headers));

    let mut headers_ns = HeaderMap::new();
    headers_ns.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    assert!(!is_response_cacheable(StatusCode::OK, &headers_ns));

    let mut headers_private = HeaderMap::new();
    headers_private.insert(CACHE_CONTROL, "private".parse().unwrap());
    assert!(!is_response_cacheable(StatusCode::OK, &headers_private));

    let empty_headers = HeaderMap::new();
    assert!(!is_response_cacheable(StatusCode::OK, &empty_headers));
}

#[test]
fn test_is_response_cacheable_with_etag() {
    let mut headers = HeaderMap::new();
    headers.insert(ETAG, "\"abc\"".parse().unwrap());
    assert!(is_response_cacheable(StatusCode::OK, &headers));
}

#[test]
fn test_is_response_cacheable_with_last_modified() {
    let mut headers = HeaderMap::new();
    headers.insert(
        LAST_MODIFIED,
        "Sun, 06 Nov 1994 08:49:37 GMT".parse().unwrap(),
    );
    assert!(is_response_cacheable(StatusCode::OK, &headers));
}

#[test]
fn test_cache_clear() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/".parse().unwrap();
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
    assert!(!cache.is_empty());
    cache.clear();
    assert!(cache.is_empty());
}

#[test]
fn test_cache_config_default() {
    let config = CacheConfig::default();
    assert_eq!(config.max_entries, 256);
}

#[test]
fn test_cache_private_not_stored() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/private".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "private, max-age=60".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );
    assert!(cache.is_empty());
}

#[test]
fn test_cache_must_revalidate() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/reval".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        CACHE_CONTROL,
        "max-age=3600, must-revalidate".parse().unwrap(),
    );
    headers.insert(ETAG, "\"v1\"".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { validators, .. } => {
            assert_eq!(validators.etag.as_deref(), Some("\"v1\""));
        }
        _ => panic!("expected stale due to must-revalidate"),
    }
}

#[test]
fn test_cache_no_cache_forces_revalidation() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/nc".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "no-cache".parse().unwrap());
    headers.insert(ETAG, "\"v2\"".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { .. } => {}
        _ => panic!("expected stale due to no-cache"),
    }
}

#[test]
fn test_cache_immutable() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/immut".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600, immutable".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(_) => {}
        _ => panic!("expected fresh for immutable entry"),
    }
}

#[test]
fn test_cache_stale_while_revalidate() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/swr".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        CACHE_CONTROL,
        "max-age=0, stale-while-revalidate=3600".parse().unwrap(),
    );
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("stale-ok"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.body, Bytes::from("stale-ok"));
        }
        _ => panic!("expected fresh via stale-while-revalidate"),
    }
}

#[test]
fn test_cache_stale_if_error_propagated() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/sie".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        CACHE_CONTROL,
        "max-age=0, stale-if-error=600".parse().unwrap(),
    );
    headers.insert(ETAG, "\"sie\"".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { stale_if_error, .. } => {
            assert_eq!(stale_if_error, Some(Duration::from_secs(600)));
        }
        _ => panic!("expected stale with stale_if_error"),
    }
}

#[test]
fn test_cache_head_method() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/head".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::HEAD,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::new(),
        &HeaderMap::new(),
    );
    assert_eq!(cache.len(), 1);
}

#[test]
fn test_cache_invalidation_on_delete() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/resource".parse().unwrap();
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
    cache.invalidate(&Method::DELETE, &uri);
    assert!(cache.is_empty());
}

#[test]
fn test_cache_get_does_not_invalidate() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/safe".parse().unwrap();
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
    cache.invalidate(&Method::GET, &uri);
    assert_eq!(cache.len(), 1);
}

#[test]
fn test_cache_s_maxage() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/smaxage".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "s-maxage=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );
    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(_) => {}
        _ => panic!("expected fresh from s-maxage"),
    }
}

#[test]
fn test_validators_apply_to_request() {
    let validators = Validators {
        etag: Some("\"abc\"".to_string()),
        last_modified: Some("Sun, 06 Nov 1994 08:49:37 GMT".to_string()),
    };
    let mut headers = HeaderMap::new();
    validators.apply_to_request(&mut headers);
    assert!(headers.contains_key(IF_NONE_MATCH));
    assert!(headers.contains_key(IF_MODIFIED_SINCE));
}

#[test]
fn test_cache_lookup_post_is_miss() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/".parse().unwrap();
    match cache.lookup(&Method::POST, &uri, &HeaderMap::new()) {
        CacheLookup::Miss => {}
        _ => panic!("expected miss for POST"),
    }
}

#[test]
fn test_cache_debug() {
    let cache = HttpCache::new();
    let dbg = format!("{cache:?}");
    assert!(dbg.contains("HttpCache"));
}

#[test]
fn test_cache_default() {
    let cache: HttpCache = Default::default();
    assert!(cache.is_empty());
}

#[test]
fn test_cache_expires_based_freshness() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/expires".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(EXPIRES, "Thu, 01 Jan 2099 00:00:00 GMT".parse().unwrap());
    let body = Bytes::from("expires-fresh");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.body, Bytes::from("expires-fresh"));
        }
        _ => panic!("expected fresh from Expires header"),
    }
}

#[test]
fn test_cache_expires_based_staleness_with_etag() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/stale-expires".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(EXPIRES, "Thu, 01 Jan 2020 00:00:00 GMT".parse().unwrap());
    headers.insert(ETAG, "\"exp-v1\"".parse().unwrap());
    let body = Bytes::from("expired");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { validators, .. } => {
            assert_eq!(validators.etag.as_deref(), Some("\"exp-v1\""));
        }
        _ => panic!("expected stale due to expired Expires header"),
    }
}

#[test]
fn test_cache_stale_without_validators_is_miss() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/no-validators".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=0".parse().unwrap());
    let body = Bytes::from("no validators");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Miss => {}
        _ => panic!("expected miss for stale entry without validators"),
    }
}

#[test]
fn cache_max_age_zero_without_validators_is_miss() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/max-age-zero-no-validators"
        .parse()
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=0".parse().unwrap());
    // No ETag or Last-Modified — no validators

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("should not be retrievable"),
        &HeaderMap::new(),
    );

    // Immediate lookup — max-age=0 means stale right away, no validators → Miss
    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Miss => {}
        CacheLookup::Fresh(_) => panic!("expected Miss, got Fresh"),
        CacheLookup::Stale { .. } => panic!("expected Miss, got Stale"),
    }
}

#[test]
fn test_cache_immutable_with_must_revalidate() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/immut-mr".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        CACHE_CONTROL,
        "max-age=3600, immutable, must-revalidate".parse().unwrap(),
    );
    let body = Bytes::from("immut");

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &body,
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.body, Bytes::from("immut"));
        }
        _ => panic!("expected fresh for immutable+must_revalidate entry"),
    }
}

#[test]
fn test_httpdate_parse_november() {
    let result = httpdate_parse("Sun, 06 Nov 1994 08:49:37 GMT");
    assert!(result.is_some());
}

#[test]
fn test_httpdate_parse_december() {
    let result = httpdate_parse("Sun, 25 Dec 2022 12:00:00 GMT");
    assert!(result.is_some());
}

#[test]
fn test_httpdate_parse_all_months() {
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    for m in &months {
        let date = format!("Mon, 15 {m} 2023 10:30:00 GMT");
        assert!(httpdate_parse(&date).is_some(), "month {m} should parse");
    }
}

#[test]
fn test_httpdate_leap_year() {
    let result = httpdate_parse("Mon, 01 Mar 2024 00:00:00 GMT");
    assert!(result.is_some());
}

#[test]
fn test_cache_config_debug() {
    let config = CacheConfig::default();
    let dbg = format!("{config:?}");
    assert!(dbg.contains("CacheConfig"));
}

#[test]
fn test_cached_response_into_http_response() {
    let cached = CachedResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("resp"),
        age: Duration::from_secs(42),
    };
    let resp = cached.into_http_response();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get(AGE).unwrap().to_str().unwrap(), "42");
}

#[test]
fn test_validators_apply_only_etag() {
    let validators = Validators {
        etag: Some("\"only-etag\"".to_string()),
        last_modified: None,
    };
    let mut headers = HeaderMap::new();
    validators.apply_to_request(&mut headers);
    assert!(headers.contains_key(IF_NONE_MATCH));
    assert!(!headers.contains_key(IF_MODIFIED_SINCE));
}

#[test]
fn test_validators_apply_only_last_modified() {
    let validators = Validators {
        etag: None,
        last_modified: Some("Sun, 06 Nov 1994 08:49:37 GMT".to_string()),
    };
    let mut headers = HeaderMap::new();
    validators.apply_to_request(&mut headers);
    assert!(!headers.contains_key(IF_NONE_MATCH));
    assert!(headers.contains_key(IF_MODIFIED_SINCE));
}

#[test]
fn test_is_response_cacheable_with_expires() {
    let mut headers = HeaderMap::new();
    headers.insert(EXPIRES, "Thu, 01 Jan 2099 00:00:00 GMT".parse().unwrap());
    assert!(is_response_cacheable(StatusCode::OK, &headers));
}

#[test]
fn test_non_cacheable_status_not_stored() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/500".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::INTERNAL_SERVER_ERROR,
        &headers,
        &Bytes::from("err"),
        &HeaderMap::new(),
    );
    assert!(cache.is_empty());
}

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

#[test]
fn test_staleness_with_expired_expires_header() {
    // Test the staleness path when expires is in the past (lines 59-67)
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/past-expires".parse().unwrap();
    let mut headers = HeaderMap::new();
    // Expires in the past (year 2020) + etag for validators
    headers.insert(EXPIRES, "Thu, 01 Jan 2020 00:00:00 GMT".parse().unwrap());
    headers.insert(ETAG, "\"expired-v1\"".parse().unwrap());

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("old data"),
        &HeaderMap::new(),
    );

    // Should be stale because expires is in the past
    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { cached, .. } => {
            assert_eq!(cached.body, Bytes::from("old data"));
        }
        CacheLookup::Fresh(_) => panic!("past expires should not be fresh"),
        CacheLookup::Miss => panic!("should find the entry (has validators)"),
    }
}

#[test]
fn test_staleness_with_max_age_zero() {
    // max-age=0 means immediately stale (covers lines 54-56)
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/immediate-stale".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=0".parse().unwrap());
    headers.insert(ETAG, "\"v1\"".parse().unwrap());

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("stale immediately"),
        &HeaderMap::new(),
    );

    // Give it a tiny bit of time to become "stale"
    std::thread::sleep(std::time::Duration::from_millis(1));

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { cached, .. } => {
            assert_eq!(cached.body, Bytes::from("stale immediately"));
        }
        CacheLookup::Fresh(_) => panic!("max-age=0 should be stale immediately"),
        CacheLookup::Miss => panic!("should find the entry (has etag)"),
    }
}

#[test]
fn test_staleness_with_future_expires_is_fresh() {
    // When expires is in the future, the entry is fresh (line 42 returns true)
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/future-expires".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(EXPIRES, "Thu, 01 Jan 2099 00:00:00 GMT".parse().unwrap());

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("future data"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(entry) => {
            assert_eq!(entry.body, Bytes::from("future data"));
        }
        _ => panic!("future expires should be fresh"),
    }
}

#[test]
fn test_staleness_via_expires_with_swr_serves_fresh() {
    // expires in the past + stale-while-revalidate: staleness() goes
    // through the expires path (lines 59-64) and serves via swr
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/exp-swr-staleness".parse().unwrap();
    let mut headers = HeaderMap::new();
    // Only expires (no max-age) so staleness() uses the expires path
    headers.insert(EXPIRES, "Thu, 01 Jan 2020 00:00:00 GMT".parse().unwrap());
    headers.insert(
        CACHE_CONTROL,
        "stale-while-revalidate=999999999".parse().unwrap(),
    );

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("swr via expires"),
        &HeaderMap::new(),
    );

    // staleness() returns Some(since_epoch - expires_since) via the expires path
    // and since staleness < swr, it should serve as Fresh
    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(entry) => {
            assert_eq!(entry.body, Bytes::from("swr via expires"));
        }
        _ => panic!("swr with past expires should still serve as fresh"),
    }
}

#[test]
fn test_vary_matching_same_headers_hit() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/vary".parse().unwrap();
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    resp_headers.insert(http::header::VARY, "Accept-Encoding".parse().unwrap());

    let mut req_headers = HeaderMap::new();
    req_headers.insert(http::header::ACCEPT_ENCODING, "gzip".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &resp_headers,
        &Bytes::from("gzip-data"),
        &req_headers,
    );

    match cache.lookup(&Method::GET, &uri, &req_headers) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.body, Bytes::from("gzip-data"));
        }
        _ => panic!("same Vary headers should cache-hit"),
    }
}

#[test]
fn test_vary_matching_different_headers_miss() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/vary-miss".parse().unwrap();
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    resp_headers.insert(http::header::VARY, "Accept-Encoding".parse().unwrap());

    let mut stored_req = HeaderMap::new();
    stored_req.insert(http::header::ACCEPT_ENCODING, "gzip".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &resp_headers,
        &Bytes::from("gzip-data"),
        &stored_req,
    );

    let mut different_req = HeaderMap::new();
    different_req.insert(http::header::ACCEPT_ENCODING, "br".parse().unwrap());
    match cache.lookup(&Method::GET, &uri, &different_req) {
        CacheLookup::Miss => {}
        _ => panic!("different Vary header value should be a cache miss"),
    }
}

/// BUG(#136): Multiple Vary variants for the same URL should coexist in cache.
#[test]
fn test_vary_multiple_variants_stored() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/resource".parse().unwrap();
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    resp_headers.insert(http::header::VARY, "Accept-Encoding".parse().unwrap());

    // Store gzip variant
    let mut gzip_req = HeaderMap::new();
    gzip_req.insert(http::header::ACCEPT_ENCODING, "gzip".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &resp_headers,
        &Bytes::from("gzip-body"),
        &gzip_req,
    );

    // Store br variant — should NOT overwrite gzip variant
    let mut br_req = HeaderMap::new();
    br_req.insert(http::header::ACCEPT_ENCODING, "br".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &resp_headers,
        &Bytes::from("br-body"),
        &br_req,
    );

    // Both variants should be retrievable
    match cache.lookup(&Method::GET, &uri, &gzip_req) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(
                resp.body,
                Bytes::from("gzip-body"),
                "gzip variant should still be cached"
            );
        }
        _ => panic!("gzip variant was overwritten by br variant"),
    }

    match cache.lookup(&Method::GET, &uri, &br_req) {
        CacheLookup::Fresh(resp) => {
            assert_eq!(resp.body, Bytes::from("br-body"));
        }
        _ => panic!("br variant should be cached"),
    }
}

#[test]
fn test_vary_star_always_misses() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/vary-star".parse().unwrap();
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
    resp_headers.insert(http::header::VARY, "*".parse().unwrap());

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &resp_headers,
        &Bytes::from("star"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Miss => {}
        _ => panic!("Vary: * should always miss"),
    }
}

#[test]
fn test_cacheable_status_codes() {
    let cacheable = [200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 414, 501];
    for code in cacheable {
        assert!(
            is_cacheable_status(StatusCode::from_u16(code).unwrap()),
            "status {code} should be cacheable"
        );
    }
    let not_cacheable = [201, 202, 302, 303, 307, 400, 401, 403, 500, 502, 503];
    for code in not_cacheable {
        assert!(
            !is_cacheable_status(StatusCode::from_u16(code).unwrap()),
            "status {code} should not be cacheable"
        );
    }
}

#[test]
fn test_cache_last_modified_validator() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/lm".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=0".parse().unwrap());
    headers.insert(
        LAST_MODIFIED,
        "Sun, 06 Nov 1994 08:49:37 GMT".parse().unwrap(),
    );

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("modified"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { validators, .. } => {
            assert_eq!(
                validators.last_modified.as_deref(),
                Some("Sun, 06 Nov 1994 08:49:37 GMT")
            );
            assert!(validators.etag.is_none());
        }
        _ => panic!("expected stale with last_modified validator"),
    }
}

#[test]
fn test_parse_cache_control_non_ascii_returns_defaults() {
    // When Cache-Control header value is non-ASCII, to_str() fails and
    // parse_cache_control returns default directives (line 509)
    let mut headers = HeaderMap::new();
    // Insert raw bytes that are not valid UTF-8
    headers.insert(
        CACHE_CONTROL,
        http::HeaderValue::from_bytes(b"max-age=3600, \xff\xfe").unwrap(),
    );
    let directives = parse_cache_control(&headers);
    // All fields should be at their defaults since parsing bailed
    assert!(directives.max_age.is_none());
    assert!(!directives.no_store);
    assert!(!directives.no_cache);
    assert!(!directives.private);
    assert!(!directives.must_revalidate);
    assert!(!directives.immutable);
    assert!(directives.stale_while_revalidate.is_none());
    assert!(directives.stale_if_error.is_none());
}

#[test]
fn test_vary_matches_with_vary_but_no_stored_headers_returns_true() {
    // When entry has `vary` set but `request_vary_headers` is None,
    // vary_matches should return true (line 629)
    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("data"),
        stored_at: Instant::now(),
        max_age: Some(Duration::from_secs(3600)),
        expires_at: None,
        etag: None,
        last_modified: None,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
        vary: Some(vec!["accept-encoding".to_string()]),
        request_vary_headers: None, // <-- this is the key: vary is Some but stored headers is None
    };
    let request_headers = HeaderMap::new();
    assert!(vary_matches(&entry, &request_headers));
}

#[test]
fn test_staleness_returns_none_when_max_age_not_exceeded() {
    // When age < max_age, staleness() returns None (covers line 57)
    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("fresh"),
        stored_at: Instant::now(), // just created, age ~0
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
    // staleness should be None since entry is fresh
    assert!(entry.staleness().is_none());
}

#[test]
fn test_staleness_returns_none_when_expires_in_future() {
    // When expires is in the future, staleness() returns None (covers lines 66-68)
    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("fresh-expires"),
        stored_at: Instant::now(),
        max_age: None, // no max-age, so staleness checks expires
        expires_at: Some(SystemTime::now() + Duration::from_secs(3600)),
        etag: None,
        last_modified: None,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
        vary: None,
        request_vary_headers: None,
    };
    // staleness should be None since expires is in the future
    assert!(entry.staleness().is_none());
}

#[test]
fn test_staleness_returns_none_when_no_max_age_no_expires() {
    // When there is neither max_age nor expires_at, staleness() returns None
    // (covers the final None at line 68)
    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("no-info"),
        stored_at: Instant::now(),
        max_age: None,
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
    assert!(entry.staleness().is_none());
}

#[test]
fn test_staleness_returns_some_when_max_age_exceeded() {
    // When age > max_age, staleness() returns Some(age - max_age)
    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("stale"),
        stored_at: Instant::now() - Duration::from_secs(120), // 120s old
        max_age: Some(Duration::from_secs(60)),               // max-age is 60s
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
    let staleness = entry.staleness();
    assert!(staleness.is_some());
    // Should be ~60s of staleness
    let staleness = staleness.unwrap();
    assert!(staleness >= Duration::from_secs(59));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_poisoned_lock_get_returns_none() {
    // Poison the mutex by panicking inside a lock scope
    let store = std::sync::Arc::new(InMemoryCacheStore::new(256));
    let store_clone = store.clone();
    let _ = std::thread::spawn(move || {
        let _lock = store_clone.inner.lock().unwrap();
        panic!("intentional panic to poison mutex");
    })
    .join();

    // Now the mutex is poisoned
    let uri: Uri = "http://example.com/poisoned".parse().unwrap();
    // get should return empty on poisoned lock
    assert!(store.get(&Method::GET, &uri).is_empty());
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_poisoned_lock_put_is_noop() {
    let store = std::sync::Arc::new(InMemoryCacheStore::new(256));
    let store_clone = store.clone();
    let _ = std::thread::spawn(move || {
        let _lock = store_clone.inner.lock().unwrap();
        panic!("intentional panic to poison mutex");
    })
    .join();

    let uri: Uri = "http://example.com/poisoned-put".parse().unwrap();
    let entry = CacheEntry {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from("data"),
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
    // put should silently return on poisoned lock
    store.put(&Method::GET, &uri, entry);
    // len should return 0 on poisoned lock
    assert_eq!(store.len(), 0);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_poisoned_lock_remove_is_noop() {
    let store = std::sync::Arc::new(InMemoryCacheStore::new(256));
    let store_clone = store.clone();
    let _ = std::thread::spawn(move || {
        let _lock = store_clone.inner.lock().unwrap();
        panic!("intentional panic to poison mutex");
    })
    .join();

    let uri: Uri = "http://example.com/poisoned-rm".parse().unwrap();
    // remove should silently return on poisoned lock
    store.remove(&Method::GET, &uri);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_poisoned_lock_clear_is_noop() {
    let store = std::sync::Arc::new(InMemoryCacheStore::new(256));
    let store_clone = store.clone();
    let _ = std::thread::spawn(move || {
        let _lock = store_clone.inner.lock().unwrap();
        panic!("intentional panic to poison mutex");
    })
    .join();

    // clear should silently return on poisoned lock
    store.clear();
    // is_empty via len should return 0 (default on poisoned lock)
    assert!(store.is_empty());
}

#[test]
fn test_cache_no_header_returns_default_directives() {
    // When there is no Cache-Control header at all, parse_cache_control
    // returns default directives (covers line 506-507)
    let headers = HeaderMap::new();
    let directives = parse_cache_control(&headers);
    assert!(directives.max_age.is_none());
    assert!(!directives.no_store);
    assert!(!directives.no_cache);
    assert!(!directives.private);
    assert!(!directives.must_revalidate);
    assert!(!directives.immutable);
}

#[test]
fn test_s_maxage_takes_precedence_over_max_age() {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "s-maxage=3600, max-age=60".parse().unwrap());
    let directives = parse_cache_control(&headers);
    assert_eq!(directives.max_age, Some(Duration::from_secs(3600)));
}

#[test]
fn test_s_maxage_precedence_reverse_order() {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "max-age=60, s-maxage=3600".parse().unwrap());
    let directives = parse_cache_control(&headers);
    assert_eq!(directives.max_age, Some(Duration::from_secs(3600)));
}

#[test]
fn test_no_cache_without_validators_not_stored() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/nc-no-val".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "no-cache, max-age=3600".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );
    assert!(
        cache.is_empty(),
        "no-cache without validators should not be stored"
    );
}

#[test]
fn test_no_cache_with_etag_is_stored() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/nc-etag".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, "no-cache".parse().unwrap());
    headers.insert(ETAG, "\"v1\"".parse().unwrap());
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("x"),
        &HeaderMap::new(),
    );
    assert_eq!(
        cache.len(),
        1,
        "no-cache with etag validator should be stored"
    );
}
