use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use http::header::{
    AGE, CACHE_CONTROL, ETAG, EXPIRES, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
};
use http::{HeaderMap, Method, StatusCode, Uri};

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) struct CacheKey {
    method: Method,
    uri: Uri,
}

/// A cached HTTP response entry stored by a [`CacheStore`].
#[derive(Clone)]
pub struct CacheEntry {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) stored_at: Instant,
    pub(crate) max_age: Option<Duration>,
    pub(crate) expires_at: Option<SystemTime>,
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
    pub(crate) must_revalidate: bool,
    pub(crate) immutable: bool,
    pub(crate) stale_while_revalidate: Option<Duration>,
    pub(crate) stale_if_error: Option<Duration>,
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

    fn staleness(&self) -> Option<Duration> {
        let age = self.age();
        if let Some(max_age) = self.max_age {
            if age > max_age {
                return Some(age - max_age);
            }
            return None;
        }
        if let Some(expires) = self.expires_at {
            if let Ok(since_epoch) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)
                && let Ok(expires_since) = expires.duration_since(SystemTime::UNIX_EPOCH)
                && since_epoch > expires_since
            {
                return Some(since_epoch - expires_since);
            }
            return None;
        }
        None
    }

    fn has_validators(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

/// Pluggable storage backend for [`HttpCache`].
///
/// Implement this trait to use a custom cache store (e.g. moka, foyer, Redis).
/// The default implementation is [`InMemoryCacheStore`].
///
/// All methods receive `&self` and must be safe to call from multiple threads.
/// Implementations should handle their own synchronization.
pub trait CacheStore: Send + Sync + 'static {
    /// Retrieve a cached entry by method and URI.
    fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry>;

    /// Store a cache entry.
    fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry);

    /// Remove entries for the given method and URI.
    fn remove(&self, method: &Method, uri: &Uri);

    /// Remove all entries.
    fn clear(&self);

    /// Number of entries currently stored.
    fn len(&self) -> usize;

    /// Whether the store is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// In-memory cache store backed by a `HashMap`.
///
/// This is the default [`CacheStore`] used by [`HttpCache`].
pub struct InMemoryCacheStore {
    inner: Mutex<InMemoryInner>,
}

struct InMemoryInner {
    entries: HashMap<CacheKey, CacheEntry>,
    max_entries: usize,
}

impl InMemoryCacheStore {
    /// Create a new in-memory store with the given maximum entry count.
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(InMemoryInner {
                entries: HashMap::new(),
                max_entries,
            }),
        }
    }
}

impl std::fmt::Debug for InMemoryCacheStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.len();
        f.debug_struct("InMemoryCacheStore")
            .field("len", &len)
            .finish()
    }
}

impl CacheStore for InMemoryCacheStore {
    fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry> {
        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };
        self.inner.lock().unwrap().entries.get(&key).cloned()
    }

    fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };
        let mut inner = self.inner.lock().unwrap();
        if inner.entries.len() >= inner.max_entries
            && !inner.entries.contains_key(&key)
            && let Some(oldest_key) = find_oldest_entry(&inner.entries)
        {
            inner.entries.remove(&oldest_key);
        }
        inner.entries.insert(key, entry);
    }

    fn remove(&self, method: &Method, uri: &Uri) {
        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };
        self.inner.lock().unwrap().entries.remove(&key);
    }

    fn clear(&self) {
        self.inner.lock().unwrap().entries.clear();
    }

    fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }
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

/// HTTP response cache with pluggable storage.
///
/// Owns cache *policy* (freshness, revalidation, Cache-Control parsing).
/// The *storage* is delegated to a [`CacheStore`] implementation.
///
/// Use [`HttpCache::new`] or [`HttpCache::with_config`] for the default
/// in-memory store, or [`HttpCache::with_store`] for a custom backend.
pub struct HttpCache {
    store: Arc<dyn CacheStore>,
}

impl Clone for HttpCache {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
        }
    }
}

impl std::fmt::Debug for HttpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCache").finish()
    }
}

impl HttpCache {
    /// Create a new cache with default settings (256 max entries, in-memory store).
    pub fn new() -> Self {
        Self::with_config(CacheConfig::default())
    }

    /// Create a cache with custom configuration using the default in-memory store.
    pub fn with_config(config: CacheConfig) -> Self {
        Self {
            store: Arc::new(InMemoryCacheStore::new(config.max_entries)),
        }
    }

    /// Create a cache with a custom [`CacheStore`] backend.
    pub fn with_store(store: impl CacheStore) -> Self {
        Self {
            store: Arc::new(store),
        }
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        self.store.clear();
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    pub(crate) fn lookup(&self, method: &Method, uri: &Uri) -> CacheLookup {
        if !is_cacheable_method(method) {
            return CacheLookup::Miss;
        }

        let Some(entry) = self.store.get(method, uri) else {
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

        // immutable entries skip revalidation while fresh
        if entry.immutable && entry.is_fresh() {
            return CacheLookup::Fresh(CachedResponse {
                status: entry.status,
                headers: entry.headers.clone(),
                body: entry.body.clone(),
                age: entry.age(),
            });
        }

        // stale-while-revalidate: serve stale content within the grace window
        if let Some(swr) = entry.stale_while_revalidate
            && let Some(staleness) = entry.staleness()
            && staleness <= swr
        {
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
                stale_if_error: entry.stale_if_error,
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
            immutable: directives.immutable,
            stale_while_revalidate: directives.stale_while_revalidate,
            stale_if_error: directives.stale_if_error,
        };

        self.store.put(method, uri, entry);
    }

    pub(crate) fn invalidate(&self, method: &Method, uri: &Uri) {
        if is_unsafe_method(method) {
            self.store.remove(&Method::GET, uri);
            self.store.remove(&Method::HEAD, uri);
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
        stale_if_error: Option<Duration>,
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
        if let Some(ref etag) = self.etag
            && let Ok(val) = etag.parse()
        {
            headers.insert(IF_NONE_MATCH, val);
        }
        if let Some(ref lm) = self.last_modified
            && let Ok(val) = lm.parse()
        {
            headers.insert(IF_MODIFIED_SINCE, val);
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
    immutable: bool,
    stale_while_revalidate: Option<Duration>,
    stale_if_error: Option<Duration>,
}

fn parse_cache_control(headers: &HeaderMap) -> CacheDirectives {
    let mut directives = CacheDirectives {
        max_age: None,
        no_store: false,
        no_cache: false,
        private: false,
        must_revalidate: false,
        immutable: false,
        stale_while_revalidate: None,
        stale_if_error: None,
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
        } else if let Some(age_str) = part.strip_prefix("max-age=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            directives.max_age = Some(Duration::from_secs(secs));
        } else if let Some(age_str) = part.strip_prefix("s-maxage=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            directives.max_age = Some(Duration::from_secs(secs));
        } else if part == "immutable" {
            directives.immutable = true;
        } else if let Some(age_str) = part.strip_prefix("stale-while-revalidate=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            directives.stale_while_revalidate = Some(Duration::from_secs(secs));
        } else if let Some(age_str) = part.strip_prefix("stale-if-error=")
            && let Ok(secs) = age_str.trim().parse::<u64>()
        {
            directives.stale_if_error = Some(Duration::from_secs(secs));
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
        );

        match cache.lookup(&Method::GET, &uri) {
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
        );

        match cache.lookup(&Method::GET, &uri) {
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
        );

        match cache.lookup(&Method::GET, &uri) {
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

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);

        match cache.lookup(&Method::GET, &uri) {
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

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);

        match cache.lookup(&Method::GET, &uri) {
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

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);

        match cache.lookup(&Method::GET, &uri) {
            CacheLookup::Miss => {}
            _ => panic!("expected miss for stale entry without validators"),
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

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);

        match cache.lookup(&Method::GET, &uri) {
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
        );

        match cache.lookup(&Method::GET, &uri) {
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
        );

        match cache.lookup(&Method::GET, &uri) {
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

        cache.store(&Method::GET, &uri, StatusCode::OK, &headers, &body);

        match cache.lookup(&Method::GET, &uri) {
            CacheLookup::Fresh(resp) => {
                assert_eq!(resp.body, Bytes::from("swr-expires"));
            }
            _ => panic!("expected fresh via stale-while-revalidate with Expires"),
        }
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
            fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry> {
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
        );

        assert_eq!(cache.len(), 1);
        assert_eq!(put_count.load(Ordering::Relaxed), 1);

        match cache.lookup(&Method::GET, &uri) {
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

        assert!(store.get(&Method::GET, &uri).is_none());
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
        };
        store.put(&Method::GET, &uri, entry);
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());

        let got = store.get(&Method::GET, &uri).unwrap();
        assert_eq!(got.body, Bytes::from("body"));

        store.remove(&Method::GET, &uri);
        assert!(store.get(&Method::GET, &uri).is_none());
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
            store.get(&Method::GET, &uri_a).is_none(),
            "oldest entry (a) should be evicted"
        );
        assert!(store.get(&Method::GET, &uri_b).is_some());
        assert!(store.get(&Method::GET, &uri_c).is_some());
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
        };

        let uri_a: Uri = "http://example.com/a".parse().unwrap();
        let uri_b: Uri = "http://example.com/b".parse().unwrap();

        store.put(&Method::GET, &uri_a, entry("a1"));
        store.put(&Method::GET, &uri_b, entry("b1"));

        store.put(&Method::GET, &uri_a, entry("a2"));
        assert_eq!(store.len(), 2);
        let got = store.get(&Method::GET, &uri_a).unwrap();
        assert_eq!(got.body, Bytes::from("a2"));
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
        };
        let head_entry = CacheEntry {
            body: Bytes::from("head-body"),
            ..entry.clone()
        };

        let uri: Uri = "http://example.com/x".parse().unwrap();
        store.put(&Method::GET, &uri, entry);
        store.put(&Method::HEAD, &uri, head_entry);
        assert_eq!(store.len(), 2);

        let get_val = store.get(&Method::GET, &uri).unwrap();
        assert_eq!(get_val.body, Bytes::from("get-body"));
        let head_val = store.get(&Method::HEAD, &uri).unwrap();
        assert_eq!(head_val.body, Bytes::from("head-body"));

        store.remove(&Method::GET, &uri);
        assert!(store.get(&Method::GET, &uri).is_none());
        assert!(store.get(&Method::HEAD, &uri).is_some());
    }

    #[test]
    fn test_custom_store_invalidate_calls_remove() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct TrackingStore {
            inner: InMemoryCacheStore,
            remove_count: Arc<AtomicUsize>,
        }

        impl CacheStore for TrackingStore {
            fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry> {
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
            fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry> {
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
        );

        match cache.lookup(&Method::GET, &uri) {
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
        );

        match cache.lookup(&Method::GET, &uri) {
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
            fn get(&self, method: &Method, uri: &Uri) -> Option<CacheEntry> {
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
        );

        assert_eq!(cache2.len(), 1);
        match cache2.lookup(&Method::GET, &uri) {
            CacheLookup::Fresh(resp) => {
                assert_eq!(resp.body, Bytes::from("shared"));
            }
            _ => panic!("cloned cache should see entries from original"),
        }
    }
}
