use std::collections::HashMap;
use std::sync::Mutex;

use http::{Method, Uri};

use super::CacheEntry;

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub(crate) struct CacheKey {
    method: Method,
    uri: Uri,
}

/// Pluggable storage backend for [`super::HttpCache`].
///
/// Implement this trait to use a custom cache store (e.g. moka, foyer, Redis).
/// The default implementation is [`InMemoryCacheStore`].
///
/// All methods receive `&self` and must be safe to call from multiple threads.
/// Implementations should handle their own synchronization.
pub trait CacheStore: Send + Sync + 'static {
    /// Retrieve all cached variants for the given method and URI.
    fn get(&self, method: &Method, uri: &Uri) -> Vec<CacheEntry>;

    /// Store a cache entry (adds or replaces the matching Vary variant).
    fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry);

    /// Remove all entries for the given method and URI.
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
/// This is the default [`CacheStore`] used by [`super::HttpCache`].
pub struct InMemoryCacheStore {
    pub(super) inner: Mutex<InMemoryInner>,
}

pub(super) struct InMemoryInner {
    entries: HashMap<CacheKey, Vec<CacheEntry>>,
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
    fn get(&self, method: &Method, uri: &Uri) -> Vec<CacheEntry> {
        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        inner.entries.get(&key).cloned().unwrap_or_default()
    }

    fn put(&self, method: &Method, uri: &Uri, entry: CacheEntry) {
        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let total_variants: usize = inner.entries.values().map(|v| v.len()).sum();
        if total_variants >= inner.max_entries
            && let Some(oldest_key) = find_oldest_entry(&inner.entries)
        {
            let should_remove_key = inner.entries.get(&oldest_key).is_none_or(|v| v.len() <= 1);
            if should_remove_key {
                inner.entries.remove(&oldest_key);
            } else if let Some(variants) = inner.entries.get_mut(&oldest_key) {
                let oldest_idx = variants
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.stored_at)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                variants.swap_remove(oldest_idx);
            }
        }
        let variants = inner.entries.entry(key).or_default();
        if let Some(existing) = variants
            .iter_mut()
            .find(|e| e.request_vary_headers == entry.request_vary_headers)
        {
            *existing = entry;
        } else {
            variants.push(entry);
        }
    }

    fn remove(&self, method: &Method, uri: &Uri) {
        let key = CacheKey {
            method: method.clone(),
            uri: uri.clone(),
        };
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.entries.remove(&key);
    }

    fn clear(&self) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.entries.clear();
    }

    fn len(&self) -> usize {
        let Ok(inner) = self.inner.lock() else {
            return 0;
        };
        inner.entries.values().map(|v| v.len()).sum()
    }
}

fn find_oldest_entry(entries: &HashMap<CacheKey, Vec<CacheEntry>>) -> Option<CacheKey> {
    entries
        .iter()
        .filter_map(|(key, variants)| variants.iter().map(|e| e.stored_at).min().map(|t| (key, t)))
        .min_by_key(|(_, t)| *t)
        .map(|(key, _)| key.clone())
}
