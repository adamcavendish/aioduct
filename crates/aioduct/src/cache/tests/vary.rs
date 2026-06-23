use super::*;

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
