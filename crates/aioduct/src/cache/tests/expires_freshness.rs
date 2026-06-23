use super::*;

#[test]
fn test_cache_expires_staleness_returns_some() {
    let store = InMemoryCacheStore::new(256);
    let uri: Uri = "http://example.com/exp-stale".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(EXPIRES, "Thu, 01 Jan 2020 00:00:00 GMT".parse().unwrap());
    headers.insert(ETAG, "\"exp-stale\"".parse().unwrap());

    let cache = HttpCache::with_store(store);
    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("data"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Stale { .. } => {}
        _ => panic!("expected stale for expired Expires"),
    }
}

#[test]
fn test_cache_expires_freshness_staleness_returns_none() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/exp-fresh".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(EXPIRES, "Thu, 01 Jan 2099 00:00:00 GMT".parse().unwrap());

    cache.store(
        &Method::GET,
        &uri,
        StatusCode::OK,
        &headers,
        &Bytes::from("data"),
        &HeaderMap::new(),
    );

    match cache.lookup(&Method::GET, &uri, &HeaderMap::new()) {
        CacheLookup::Fresh(_) => {}
        _ => panic!("expected fresh for future Expires"),
    }
}

#[test]
fn test_cache_expires_stale_while_revalidate_serves_within_grace() {
    let cache = HttpCache::new();
    let uri: Uri = "http://example.com/exp-swr".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(EXPIRES, "Thu, 01 Jan 2020 00:00:00 GMT".parse().unwrap());
    headers.insert(
        CACHE_CONTROL,
        "stale-while-revalidate=999999999".parse().unwrap(),
    );
    let body = Bytes::from("swr-expires");

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
            assert_eq!(resp.body, Bytes::from("swr-expires"));
        }
        _ => panic!("expected fresh via stale-while-revalidate with Expires"),
    }
}
