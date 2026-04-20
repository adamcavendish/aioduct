use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use http::header::{
    AGE, CACHE_CONTROL, ETAG, EXPIRES, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
};
use http::{HeaderMap, Method, StatusCode, Uri};

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct CacheKey {
    method: Method,
    uri: Uri,
}

#[derive(Clone)]
struct CacheEntry {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    stored_at: Instant,
    max_age: Option<Duration>,
    expires_at: Option<SystemTime>,
    etag: Option<String>,
    last_modified: Option<String>,
    must_revalidate: bool,
}

impl CacheEntry {
    fn is_fresh(&self) -> bool {
        if let Some(max_age) = self.max_age {
            return self.stored_at.elapsed() < max_age;
        }
        if let Some(expires) = self.expires_at {
            return SystemTime::now() < expires;
        }
        false
    }

    fn age(&self) -> Duration {
        self.stored_at.elapsed()
    }

    fn has_validators(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

/// In-memory HTTP response cache.
///
/// Caches responses based on `Cache-Control`, `Expires`, `ETag`, and
/// `Last-Modified` headers. Supports conditional validation via
/// `If-None-Match` and `If-Modified-Since`.
///
/// Only `GET` and `HEAD` responses with cacheable status codes are stored.
#[derive(Clone)]
pub struct HttpCache {
    inner: Arc<Mutex<CacheInner>>,
}

struct CacheInner {
    entries: HashMap<CacheKey, CacheEntry>,
    max_entries: usize,
}

/// Configuration for the HTTP cache.
#[derive(Clone, Debug)]
pub struct CacheConfig {
    /// Maximum number of entries the cache can hold.
    pub max_entries: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self { max_entries: 256 }
    }
}

impl std::fmt::Debug for HttpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCache").finish()
    }
}

impl HttpCache {
    /// Create a new cache with default settings (256 max entries).
    pub fn new() -> Self {
        Self::with_config(CacheConfig::default())
    }

    /// Create a cache with custom configuration.
    pub fn with_config(config: CacheConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CacheInner {
                entries: HashMap::new(),
                max_entries: config.max_entries,
            })),
        }
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        self.inner.lock().unwrap().entries.clear();
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn lookup(&self, method: &Method, uri: &Uri) -> CacheLookup {
        if !is_cacheable_method(method) {
            return CacheLookup::Miss;
        }

        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };

        let inner = self.inner.lock().unwrap();
        let Some(entry) = inner.entries.get(&key) else {
            return CacheLookup::Miss;
        };

        if entry.is_fresh() && !entry.must_revalidate {
            return CacheLookup::Fresh(CachedResponse {
                status: entry.status,
                headers: entry.headers.clone(),
                body: entry.body.clone(),
                age: entry.age(),
            });
        }

        if entry.has_validators() {
            return CacheLookup::Stale {
                validators: Validators {
                    etag: entry.etag.clone(),
                    last_modified: entry.last_modified.clone(),
                },
                cached: CachedResponse {
                    status: entry.status,
                    headers: entry.headers.clone(),
                    body: entry.body.clone(),
                    age: entry.age(),
                },
            };
        }

        CacheLookup::Miss
    }

    pub(crate) fn store(
        &self,
        method: &Method,
        uri: &Uri,
        status: StatusCode,
        headers: &HeaderMap,
        body: &Bytes,
    ) {
        if !is_cacheable_method(method) || !is_cacheable_status(status) {
            return;
        }

        let directives = parse_cache_control(headers);

        if directives.no_store || directives.private {
            return;
        }

        let entry = CacheEntry {
            status,
            headers: headers.clone(),
            body: body.clone(),
            stored_at: Instant::now(),
            max_age: directives.max_age,
            expires_at: if directives.max_age.is_none() {
                parse_expires(headers)
            } else {
                None
            },
            etag: headers
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(String::from),
            last_modified: headers
                .get(LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(String::from),
            must_revalidate: directives.must_revalidate,
        };

        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };

        let mut inner = self.inner.lock().unwrap();

        if inner.entries.len() >= inner.max_entries && !inner.entries.contains_key(&key) {
            if let Some(oldest_key) = find_oldest_entry(&inner.entries) {
                inner.entries.remove(&oldest_key);
            }
        }

        inner.entries.insert(key, entry);
    }

    pub(crate) fn invalidate(&self, method: &Method, uri: &Uri) {
        if is_unsafe_method(method) {
            let key = CacheKey {
                method: Method::GET,
                uri: uri.clone(),
            };
            let mut inner = self.inner.lock().unwrap();
            inner.entries.remove(&key);
            let head_key = CacheKey {
                method: Method::HEAD,
                uri: uri.clone(),
            };
            inner.entries.remove(&head_key);
        }
    }
}

impl Default for HttpCache {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) enum CacheLookup {
    Fresh(CachedResponse),
    Stale {
        validators: Validators,
        cached: CachedResponse,
    },
    Miss,
}

pub(crate) struct CachedResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub age: Duration,
}

pub(crate) struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validators {
    pub fn apply_to_request(&self, headers: &mut HeaderMap) {
        if let Some(ref etag) = self.etag {
            if let Ok(val) = etag.parse() {
                headers.insert(IF_NONE_MATCH, val);
            }
        }
        if let Some(ref lm) = self.last_modified {
            if let Ok(val) = lm.parse() {
                headers.insert(IF_MODIFIED_SINCE, val);
            }
        }
    }
}

impl CachedResponse {
    pub fn into_http_response(self) -> http::Response<crate::error::AioductBody> {
        use http_body_util::BodyExt;

        let mut builder = http::Response::builder().status(self.status);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        if let Ok(age_secs) = http::HeaderValue::from_str(&self.age.as_secs().to_string()) {
            builder = builder.header(AGE, age_secs);
        }
        builder
            .body(
                http_body_util::Full::new(self.body)
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .expect("cached response build should not fail")
    }
}

struct CacheDirectives {
    max_age: Option<Duration>,
    no_store: bool,
    no_cache: bool,
    private: bool,
    must_revalidate: bool,
}

fn parse_cache_control(headers: &HeaderMap) -> CacheDirectives {
    let mut directives = CacheDirectives {
        max_age: None,
        no_store: false,
        no_cache: false,
        private: false,
        must_revalidate: false,
    };

    let Some(value) = headers.get(CACHE_CONTROL) else {
        return directives;
    };
    let Ok(s) = value.to_str() else {
        return directives;
    };

    for part in s.split(',') {
        let part = part.trim().to_lowercase();
        if part == "no-store" {
            directives.no_store = true;
        } else if part == "no-cache" {
            directives.no_cache = true;
            directives.must_revalidate = true;
        } else if part == "private" {
            directives.private = true;
        } else if part == "must-revalidate" {
            directives.must_revalidate = true;
        } else if let Some(age_str) = part.strip_prefix("max-age=") {
            if let Ok(secs) = age_str.trim().parse::<u64>() {
                directives.max_age = Some(Duration::from_secs(secs));
            }
        } else if let Some(age_str) = part.strip_prefix("s-maxage=") {
            if let Ok(secs) = age_str.trim().parse::<u64>() {
                directives.max_age = Some(Duration::from_secs(secs));
            }
        }
    }

    directives
}

fn parse_expires(headers: &HeaderMap) -> Option<SystemTime> {
    let value = headers.get(EXPIRES)?;
    let s = value.to_str().ok()?;
    httpdate_parse(s)
}

fn httpdate_parse(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    // RFC 7231 date format: "Sun, 06 Nov 1994 08:49:37 GMT"
    // Simplified parser — handles the most common format
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }

    let day: u32 = parts[1].parse().ok()?;
    let month = match parts[2].to_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: i32 = parts[3].parse().ok()?;
    let time_parts: Vec<&str> = parts[4].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let minute: u32 = time_parts[1].parse().ok()?;
    let second: u32 = time_parts[2].parse().ok()?;

    // Convert to duration since UNIX_EPOCH using a simplified calculation
    let days_since_epoch = days_from_civil(year, month, day)?;
    let secs =
        days_since_epoch as u64 * 86400 + hour as u64 * 3600 + minute as u64 * 60 + second as u64;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn days_from_civil(y: i32, m: u32, d: u32) -> Option<i64> {
    let y = y as i64;
    let m = m as i64;
    let d = d as i64;
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = ((153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1) as u64;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe as i64 - 719468)
}

fn is_cacheable_method(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

fn is_cacheable_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        200 | 203 | 204 | 206 | 300 | 301 | 308 | 404 | 405 | 410 | 414 | 501
    )
}

fn is_unsafe_method(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

pub(crate) fn is_response_cacheable(status: StatusCode, headers: &HeaderMap) -> bool {
    if !is_cacheable_status(status) {
        return false;
    }
    let directives = parse_cache_control(headers);
    if directives.no_store || directives.private {
        return false;
    }
    directives.max_age.is_some()
        || headers.contains_key(EXPIRES)
        || headers.contains_key(ETAG)
        || headers.contains_key(LAST_MODIFIED)
}

fn find_oldest_entry(entries: &HashMap<CacheKey, CacheEntry>) -> Option<CacheKey> {
    entries
        .iter()
        .min_by_key(|(_, entry)| entry.stored_at)
        .map(|(key, _)| key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_store_and_fresh_lookup() {
        let cache = HttpCache::new();
        let uri: Uri = "http://example.com/test".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
        let body = Bytes::from("hello");

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);

        match cache.lookup(&Method::GET, &uri) {
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

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);
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

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);

        match cache.lookup(&Method::GET, &uri) {
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

        cache.store(&Method::POST, &uri, StatusCode::OK, &headers, &body);
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_invalidation_on_post() {
        let cache = HttpCache::new();
        let uri: Uri = "http://example.com/resource".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
        let body = Bytes::from("data");

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);
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
        );

        match cache.lookup(&Method::GET, &uri) {
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
        );

        match cache.lookup(&Method::GET, &uri) {
            CacheLookup::Stale { .. } => {}
            _ => panic!("expected stale due to no-cache"),
        }
    }

    #[test]
    fn test_cache_head_method() {
        let cache = HttpCache::new();
        let uri: Uri = "http://example.com/head".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(CACHE_CONTROL, "max-age=3600".parse().unwrap());
        cache.store(&Method::HEAD, &uri, StatusCode::OK, &headers, &Bytes::new());
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
        );
        match cache.lookup(&Method::GET, &uri) {
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
        match cache.lookup(&Method::POST, &uri) {
            CacheLookup::Miss => {}
            _ => panic!("expected miss for POST"),
        }
    }
}
