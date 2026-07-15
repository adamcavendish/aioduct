use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use http::header::{AGE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use http::{HeaderMap, Method, StatusCode, Uri};
mod headers;
mod policy;
mod store;

#[cfg(any(not(target_arch = "wasm32"), test))]
pub(crate) use policy::is_response_cacheable;
pub use store::{CacheStore, InMemoryCacheStore};

#[cfg(test)]
use headers::httpdate_parse;
use headers::{parse_cache_control, parse_expires};
use policy::{is_cacheable_method, is_cacheable_status, is_unsafe_method, vary_matches};

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
    pub(crate) vary: Option<Vec<String>>,
    pub(crate) request_vary_headers: Option<Vec<(String, Option<String>)>>,
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

    pub(crate) fn lookup(
        &self,
        method: &Method,
        uri: &Uri,
        request_headers: &HeaderMap,
    ) -> CacheLookup {
        if !is_cacheable_method(method) {
            return CacheLookup::Miss;
        }

        let entries = self.store.get(method, uri);
        if entries.is_empty() {
            return CacheLookup::Miss;
        }

        let entry = entries
            .into_iter()
            .find(|e| vary_matches(e, request_headers));
        let Some(entry) = entry else {
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
        request_headers: &HeaderMap,
    ) {
        if !is_cacheable_method(method) || !is_cacheable_status(status) {
            return;
        }

        let directives = parse_cache_control(headers);

        if directives.no_store || directives.private {
            return;
        }

        let has_validators = headers.contains_key(ETAG) || headers.contains_key(LAST_MODIFIED);
        if directives.no_cache && !has_validators {
            return;
        }

        let vary = headers
            .get(http::header::VARY)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_lowercase())
                    .collect::<Vec<_>>()
            });

        let request_vary_headers = vary.as_ref().map(|vary_names| {
            vary_names
                .iter()
                .map(|name| {
                    let val = http::header::HeaderName::from_bytes(name.as_bytes())
                        .ok()
                        .and_then(|hn| request_headers.get(&hn))
                        .and_then(|v| v.to_str().ok())
                        .map(String::from);
                    (name.clone(), val)
                })
                .collect::<Vec<_>>()
        });

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
            vary,
            request_vary_headers,
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

#[derive(Clone)]
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
    pub fn into_http_response(self) -> http::Response<crate::body::RequestBodySend> {
        use http_body_util::BodyExt;

        let mut builder = http::Response::builder().status(self.status);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        if let Ok(age_secs) = http::HeaderValue::from_str(&self.age.as_secs().to_string()) {
            builder = builder.header(AGE, age_secs);
        }
        // SAFETY: the builder uses a valid status code and headers that were
        // already validated when the response was originally received and cached.
        #[allow(clippy::expect_used)]
        builder
            .body(
                http_body_util::Full::new(self.body)
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .expect("cached response build should not fail")
    }
}

#[cfg(test)]
mod tests;
